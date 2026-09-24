//! The host's ESI client. One instance is shared by everything in the
//! process, so rate limits, the error budget and the cache are shared too.

use eve_esi_client::Client;
use eve_esi_client::types::UniverseNamesPostItemCategory as Category;
use tether_core::tiers::EntityKind;

/// ESI accepts at most this many ids per affiliation request.
const AFFILIATION_BATCH: usize = 1000;

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

/// A named entity of any kind; `kind` is set for alliances and
/// corporations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedEntity {
    pub id: i64,
    pub name: String,
    pub kind: Option<EntityKind>,
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
        Ok(Self { client })
    }

    /// Current corporation and alliance for each character (public, bulk).
    pub async fn affiliations(
        &self,
        character_ids: &[i64],
    ) -> Result<Vec<CharacterAffiliation>, EsiError> {
        let mut out = Vec::with_capacity(character_ids.len());
        for batch in character_ids.chunks(AFFILIATION_BATCH) {
            let response = self
                .client
                .post_characters_affiliation()
                .body(batch.to_vec())
                .send()
                .await?
                .into_inner();
            out.extend(response.iter().map(|a| CharacterAffiliation {
                character_id: a.character_id,
                corporation_id: a.corporation_id,
                alliance_id: a.alliance_id,
            }));
        }
        Ok(out)
    }

    /// Resolves exact names to alliance and corporation ids (public).
    pub async fn resolve_names(&self, names: &[String]) -> Result<ResolvedNames, EsiError> {
        let body = names
            .iter()
            .map(|n| n.parse::<eve_esi_client::types::PostUniverseIdsBodyItem>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| EsiError::InvalidInput(err.to_string()))?;
        let response = self
            .client
            .post_universe_ids()
            .body(body)
            .send()
            .await?
            .into_inner();
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
    /// as ESI does.
    pub async fn names(&self, ids: &[i64]) -> Result<Vec<NamedEntity>, EsiError> {
        let response = self
            .client
            .post_universe_names()
            .body(ids.to_vec())
            .send()
            .await?
            .into_inner();
        Ok(response
            .iter()
            .map(|n| NamedEntity {
                id: n.id,
                name: n.name.clone(),
                kind: match n.category {
                    Category::Alliance => Some(EntityKind::Alliance),
                    Category::Corporation => Some(EntityKind::Corporation),
                    _ => None,
                },
            })
            .collect())
    }

    /// Players online, from `GET /status`: the cheapest proof ESI answers.
    pub async fn players_online(&self) -> Result<i64, EsiError> {
        Ok(self.client.get_status().send().await?.into_inner().players)
    }
}
