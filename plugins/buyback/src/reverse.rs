//! Reverse buyback (aa-buybackprogram's reverse programs): built next.

use std::collections::HashSet;

use tether_plugin_sdk::jobs::JobError;
use tether_plugin_sdk::{Page, PageError, Request, Submission, SubmitResult};

use crate::Access;
use crate::sync::Fetched;

pub fn render(_access: &Access, _request: &Request) -> Result<Page, PageError> {
    Err(PageError::NotFound)
}

pub fn submit(_access: &Access, _s: &Submission) -> Result<SubmitResult, PageError> {
    Err(PageError::NotFound)
}

pub fn sync_all() -> Result<(), JobError> {
    Ok(())
}

pub fn match_contracts(_fetched: &[Fetched], _matched: &mut HashSet<i64>) -> Result<(), JobError> {
    Ok(())
}

pub fn checks_and_notices(_contract_id: i64) -> Result<(), JobError> {
    Ok(())
}
