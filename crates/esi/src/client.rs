//! The host's ESI client. One instance is shared by everything in the
//! process, so rate limits, the error budget and the cache are shared too.

use eve_esi_client::Client;

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
}
