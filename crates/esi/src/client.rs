//! The host's ESI client. One instance is shared by everything in the
//! process, so eve-esi-client's rate limits, error-limit backoff and
//! Expires/ETag cache are shared too. On top, every response feeds the
//! [`Budget`], and bulk work goes through a gate so interactive requests
//! never queue behind it.

use std::future::Future;
use std::sync::Arc;
use std::time::SystemTime;

use eve_esi_client::types::UniverseNamesPostItemCategory as Category;
use eve_esi_client::{Client, ResponseValue};
use tether_core::tiers::EntityKind;
use tokio::sync::Semaphore;

use crate::budget::{Budget, BudgetSnapshot};

/// ESI accepts at most this many ids per affiliation or names request.
const BATCH: usize = 1000;
/// Bulk requests in flight at once.
const BULK_CONCURRENCY: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum EsiError {
    #[error("building the ESI client: {0}")]
    Config(String),
    #[error("ESI returned HTTP {0}")]
    Status(u16),
    #[error("ESI request failed: {0}")]
    Unavailable(String),
    #[error("invalid ESI request: {0}")]
    InvalidInput(String),
}

impl<E: std::fmt::Debug> From<eve_esi_client::Error<E>> for EsiError {
    fn from(err: eve_esi_client::Error<E>) -> Self {
        match err.status() {
            Some(status) => Self::Status(status.as_u16()),
            None => Self::Unavailable(err.to_string()),
        }
    }
}

/// Who is waiting on a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// A person, right now (a page, a login). Never gated.
    Interactive,
    /// Syncs and jobs: limited concurrency, and they back off first when
    /// ESI's error budget runs low.
    Bulk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharacterAffiliation {
    pub character_id: i64,
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
}

/// An alliance or corporation, by id and name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entity {
    pub id: i64,
    pub name: String,
}

/// A named entity of any kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedEntity {
    pub id: i64,
    pub name: String,
    /// ESI's category: `alliance`, `corporation`, `character`, ...
    pub category: String,
}

impl NamedEntity {
    /// Set for alliances and corporations.
    pub fn kind(&self) -> Option<EntityKind> {
        EntityKind::parse(&self.category)
    }
}

/// Alliances and corporations whose names matched exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedNames {
    pub alliances: Vec<Entity>,
    pub corporations: Vec<Entity>,
}

#[derive(Clone, Debug)]
pub struct Esi {
    client: Client,
    budget: Arc<Budget>,
    bulk: Arc<Semaphore>,
    /// For clients carrying a character's token (plugin calls).
    user_agent: String,
    allow: tether_net::Allowlist,
}

impl Esi {
    /// `user_agent` identifies this instance to CCP. `base_url` overrides
    /// ESI's address (tests point it at a mock server).
    pub fn new(user_agent: &str, base_url: Option<&str>) -> Result<Self, EsiError> {
        let mut builder = Client::builder().user_agent(user_agent);
        if let Some(url) = base_url {
            builder = builder.base_url(url);
        }
        let client = builder
            .build()
            .map_err(|err| EsiError::Config(err.to_string()))?;
        let allow = match base_url {
            Some(url) => tether_net::Allowlist::production().with_url(url),
            None => tether_net::Allowlist::production(),
        };
        Ok(Self {
            client,
            budget: Arc::default(),
            bulk: Arc::new(Semaphore::new(BULK_CONCURRENCY)),
            user_agent: user_agent.to_owned(),
            allow,
        })
    }

    pub fn budget(&self) -> BudgetSnapshot {
        self.budget.snapshot()
    }

    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    pub(crate) fn user_agent(&self) -> &str {
        &self.user_agent
    }

    pub(crate) fn allowlist(&self) -> &tether_net::Allowlist {
        &self.allow
    }

    /// Runs one request: gates bulk work, then records what ESI said.
    async fn call<T, E, F>(&self, priority: Priority, request: F) -> Result<T, EsiError>
    where
        E: std::fmt::Debug,
        F: Future<Output = Result<ResponseValue<T>, eve_esi_client::Error<E>>>,
    {
        self.call_full(priority, request)
            .await
            .map(ResponseValue::into_inner)
    }

    /// [`Esi::call`], keeping the response's headers (`X-Pages`).
    pub(crate) async fn call_full<T, E, F>(
        &self,
        priority: Priority,
        request: F,
    ) -> Result<ResponseValue<T>, EsiError>
    where
        E: std::fmt::Debug,
        F: Future<Output = Result<ResponseValue<T>, eve_esi_client::Error<E>>>,
    {
        let _permit = match priority {
            Priority::Interactive => None,
            Priority::Bulk => {
                let permit = self
                    .bulk
                    .acquire()
                    .await
                    .map_err(|_| EsiError::Unavailable("ESI client shut down".into()))?;
                if let Some(wait) = self.budget.bulk_delay(SystemTime::now()) {
                    tracing::warn!(
                        wait_secs = wait.as_secs(),
                        "ESI error budget low; bulk work waits for the window to reset"
                    );
                    tokio::time::sleep(wait).await;
                }
                Some(permit)
            }
        };
        match request.await {
            Ok(response) => {
                self.budget.observe(response.status(), response.headers());
                Ok(response)
            }
            Err(err) => {
                match &err {
                    eve_esi_client::Error::ErrorResponse(response) => {
                        self.budget.observe(response.status(), response.headers());
                    }
                    eve_esi_client::Error::UnexpectedResponse(response) => {
                        self.budget.observe(response.status(), response.headers());
                    }
                    _ => self.budget.observe_transport_error(),
                }
                Err(err.into())
            }
        }
    }

    /// Current corporation and alliance for each character (public, bulk).
    pub async fn affiliations(
        &self,
        character_ids: &[i64],
        priority: Priority,
    ) -> Result<Vec<CharacterAffiliation>, EsiError> {
        let mut out = Vec::with_capacity(character_ids.len());
        for batch in character_ids.chunks(BATCH) {
            let request = self
                .client
                .post_characters_affiliation()
                .body(batch.to_vec())
                .send();
            let response = self.call(priority, request).await?;
            out.extend(response.iter().map(|a| CharacterAffiliation {
                character_id: a.character_id,
                corporation_id: a.corporation_id,
                alliance_id: a.alliance_id,
            }));
        }
        Ok(out)
    }

    /// Resolves exact names to alliance and corporation ids (public).
    pub async fn resolve_names(
        &self,
        names: &[String],
        priority: Priority,
    ) -> Result<ResolvedNames, EsiError> {
        let body = names
            .iter()
            .map(|n| n.parse::<eve_esi_client::types::PostUniverseIdsBodyItem>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| EsiError::InvalidInput(err.to_string()))?;
        let request = self.client.post_universe_ids().body(body).send();
        let response = self.call(priority, request).await?;
        let entities = |items: Vec<(Option<i64>, Option<String>)>| {
            items
                .into_iter()
                .filter_map(|(id, name)| {
                    Some(Entity {
                        id: id?,
                        name: name?,
                    })
                })
                .collect()
        };
        Ok(ResolvedNames {
            alliances: entities(
                response
                    .alliances
                    .into_iter()
                    .map(|a| (a.id, a.name))
                    .collect(),
            ),
            corporations: entities(
                response
                    .corporations
                    .into_iter()
                    .map(|c| (c.id, c.name))
                    .collect(),
            ),
        })
    }

    /// Names for ids of any kind (public). Unknown ids fail the whole call,
    /// as ESI does. Prefer [`crate::names::resolve`], which caches.
    pub async fn names(
        &self,
        ids: &[i64],
        priority: Priority,
    ) -> Result<Vec<NamedEntity>, EsiError> {
        let mut out = Vec::with_capacity(ids.len());
        for batch in ids.chunks(BATCH) {
            let request = self
                .client
                .post_universe_names()
                .body(batch.to_vec())
                .send();
            let response = self.call(priority, request).await?;
            out.extend(response.iter().map(|n| NamedEntity {
                id: n.id,
                name: n.name.clone(),
                category: category(&n.category).to_owned(),
            }));
        }
        Ok(out)
    }

    /// A corporation's ticker (public).
    pub async fn corporation_ticker(
        &self,
        id: i64,
        priority: Priority,
    ) -> Result<String, EsiError> {
        let request = self
            .client
            .get_corporations_corporation_id()
            .corporation_id(id)
            .send();
        Ok(self.call(priority, request).await?.ticker)
    }

    /// An alliance's ticker (public).
    pub async fn alliance_ticker(&self, id: i64, priority: Priority) -> Result<String, EsiError> {
        let request = self
            .client
            .get_alliances_alliance_id()
            .alliance_id(id)
            .send();
        Ok(self.call(priority, request).await?.ticker)
    }

    /// Players online, from `GET /status`: the cheapest proof ESI answers.
    pub async fn players_online(&self) -> Result<i64, EsiError> {
        let request = self.client.get_status().send();
        Ok(self.call(Priority::Interactive, request).await?.players)
    }
}

fn category(c: &Category) -> &'static str {
    match c {
        Category::Alliance => "alliance",
        Category::Character => "character",
        Category::Constellation => "constellation",
        Category::Corporation => "corporation",
        Category::InventoryType => "inventory_type",
        Category::Region => "region",
        Category::SolarSystem => "solar_system",
        Category::Station => "station",
        Category::Faction => "faction",
    }
}
