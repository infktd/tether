//! Statistics (aa-buybackprogram `views/stats.py`): built next.

use tether_plugin_sdk::{Page, PageError, Request, Submission, SubmitResult};

use crate::Access;

pub fn mine(_access: &Access) -> Result<Page, PageError> {
    Err(PageError::NotFound)
}

pub fn programs(_access: &Access, _all: bool) -> Result<Page, PageError> {
    Err(PageError::NotFound)
}

pub fn details(_access: &Access, _number: &str) -> Result<Page, PageError> {
    Err(PageError::NotFound)
}

pub fn leaderboard(_access: &Access, _id: i64, _request: &Request) -> Result<Page, PageError> {
    Err(PageError::NotFound)
}

pub fn performance(_access: &Access, _id: i64) -> Result<Page, PageError> {
    Err(PageError::NotFound)
}

pub fn submit(_access: &Access, _s: &Submission) -> Result<SubmitResult, PageError> {
    Err(PageError::NotFound)
}
