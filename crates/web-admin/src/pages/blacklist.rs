//! `/blacklist`: the Blacklist and the Pilot Log (allianceauth-blacklist).

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use tether_db::blacklist::{self as db, Comment, Filter, Note};

use super::toolbar::{ListQuery, ToolbarView};
use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::blacklist::{self, Access, Flags, NewNote};
use crate::error::AppError;

pub fn when(at: &chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d %H:%M").to_string()
}

/// A Blacklist row.
pub struct Listed {
    pub note: Note,
    /// `None`: restricted, and the viewer lacks the tier (AA shows who to
    /// ask instead).
    pub reason: Option<String>,
}

/// A Pilot Log row with the comments the viewer may see.
pub struct NoteView {
    pub note: Note,
    pub comments: Vec<Comment>,
}

#[derive(Template)]
#[template(path = "blacklist.html")]
struct BlacklistPage {
    shell: Shell,
    toolbar: ToolbarView,
    /// Under the toolbar's search or filter.
    searched: bool,
    access: Access,
    listed: Vec<Listed>,
    notes: Vec<NoteView>,
    error: Option<String>,
}

/// The toolbar's search (a name, in the Blacklist and the Pilot Log alike)
/// and what the entries are about.
#[derive(Debug, Default, Deserialize)]
pub struct NotesQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    kind: String,
}

const KINDS: [(&str, &str); 4] = [
    ("character", "Pilots"),
    ("corporation", "Corporations"),
    ("alliance", "Alliances"),
    ("faction", "Factions"),
];

/// Signed in, with any of the Blacklist page's permissions.
async fn guard(
    state: &AppState,
    session: Option<CurrentSession>,
) -> Result<(CurrentSession, Shell), PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let held = tether_db::permissions::effective(&state.db, session.account).await?;
    if !tether_core::permissions::BLACKLIST_PAGE
        .iter()
        .any(|p| held.contains(*p))
    {
        return Err(AppError::forbidden().into());
    }
    let loaded = load(state, &session, "blacklist").await?;
    Ok((session, loaded.shell))
}

async fn page(
    state: &AppState,
    session: &CurrentSession,
    shell: Shell,
    query: &NotesQuery,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let access = blacklist::access(state, session.account).await?;
    let q: String = query.q.trim().chars().take(100).collect();
    let kind = KINDS
        .iter()
        .find(|(value, _)| *value == query.kind.trim())
        .map(|(value, _)| *value);
    let list = ListQuery::new("/blacklist")
        .param("q", &q)
        .param("kind", kind.unwrap_or(""));
    let listed = if access.blacklist {
        db::notes(
            &state.db,
            &access.reader,
            Filter {
                blacklist: true,
                search: (!q.is_empty()).then_some(q.as_str()),
                kind,
                ..Filter::default()
            },
            500,
        )
        .await?
        .into_iter()
        .map(|note| Listed {
            reason: access.reader.reads_reason(&note).then(|| note.note.clone()),
            note,
        })
        .collect()
    } else {
        Vec::new()
    };
    let notes = if access.notes() {
        db::notes(
            &state.db,
            &access.reader,
            Filter {
                search: (!q.is_empty()).then_some(q.as_str()),
                kind,
                ..Filter::default()
            },
            200,
        )
        .await?
    } else {
        Vec::new()
    };
    let comments = if access.comments && !notes.is_empty() {
        let ids: Vec<i64> = notes.iter().map(|n| n.id).collect();
        db::comments(
            &state.db,
            &ids,
            access.restricted_comments,
            access.ultra_comments,
        )
        .await?
    } else {
        Vec::new()
    };
    let notes = notes
        .into_iter()
        .map(|note| NoteView {
            comments: comments
                .iter()
                .filter(|c| c.note_id == note.id)
                .cloned()
                .collect(),
            note,
        })
        .collect();
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    Ok(super::with_problem(
        problem,
        render(
            code,
            &BlacklistPage {
                shell,
                toolbar: ToolbarView::new(&list)
                    .search("Search names")
                    .filter(&list, "About", "kind", KINDS),
                searched: list.href() != list.path,
                access,
                listed,
                notes,
                error: error.map(|e| e.message().to_owned()),
            },
        ),
    ))
}

/// `GET /blacklist`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(query): Query<NotesQuery>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session).await?;
    page(&state, &session, shell, &query, None).await
}

async fn done<T>(
    state: &AppState,
    session: &CurrentSession,
    shell: Shell,
    result: Result<T, AppError>,
    message: &str,
) -> Result<Response, PageError> {
    match result {
        Ok(_) => Ok(super::stay::back("/blacklist", message)),
        Err(err) => page(state, session, shell, &NotesQuery::default(), Some(err)).await,
    }
}

fn on(value: &Option<String>) -> bool {
    value.is_some()
}

#[derive(Debug, Deserialize)]
pub struct NoteForm {
    who: String,
    #[serde(default, alias = "note")]
    reason: String,
    #[serde(default)]
    blacklisted: Option<String>,
    #[serde(default)]
    restricted: Option<String>,
    #[serde(default)]
    ultra_restricted: Option<String>,
    #[serde(default)]
    linked: Option<String>,
}

impl NoteForm {
    fn new_note(&self, blacklisted: bool) -> NewNote {
        NewNote {
            who: self.who.clone(),
            reason: self.reason.clone(),
            flags: Flags {
                blacklisted: blacklisted || on(&self.blacklisted),
                restricted: on(&self.restricted),
                ultra_restricted: on(&self.ultra_restricted),
            },
            linked: on(&self.linked),
        }
    }
}

/// `POST /blacklist`: a note with Blacklist ticked.
pub async fn add(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<NoteForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session).await?;
    let result = blacklist::add_note(&state, session.account, &form.new_note(true)).await;
    done(&state, &session, shell, result, "Blacklisted.").await
}

/// `POST /blacklist/{entity_id}/remove`
pub async fn remove(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(entity_id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session).await?;
    let result = blacklist::unblacklist(&state, session.account, entity_id).await;
    done(&state, &session, shell, result, "Taken off the Blacklist.").await
}

/// `POST /blacklist/notes`
pub async fn add_note(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<NoteForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session).await?;
    let result = blacklist::add_note(&state, session.account, &form.new_note(false)).await;
    done(&state, &session, shell, result, "Note added.").await
}

#[derive(Debug, Deserialize)]
pub struct EditForm {
    reason: String,
    #[serde(default)]
    blacklisted: Option<String>,
    #[serde(default)]
    restricted: Option<String>,
    #[serde(default)]
    ultra_restricted: Option<String>,
}

/// `POST /blacklist/notes/{id}/edit`
pub async fn edit_note(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<EditForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session).await?;
    let flags = Flags {
        blacklisted: on(&form.blacklisted),
        restricted: on(&form.restricted),
        ultra_restricted: on(&form.ultra_restricted),
    };
    let result = blacklist::edit_note(&state, session.account, id, &form.reason, flags).await;
    done(&state, &session, shell, result, "Saved.").await
}

/// `POST /blacklist/notes/{id}/delete`
pub async fn delete_note(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session).await?;
    let result = blacklist::delete_note(&state, session.account, id).await;
    done(&state, &session, shell, result, "Note deleted.").await
}

#[derive(Debug, Deserialize)]
pub struct CommentForm {
    comment: String,
    #[serde(default)]
    restricted: Option<String>,
    #[serde(default)]
    ultra_restricted: Option<String>,
}

/// `POST /blacklist/notes/{id}/comments`
pub async fn add_comment(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<CommentForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session).await?;
    let flags = Flags {
        blacklisted: false,
        restricted: on(&form.restricted),
        ultra_restricted: on(&form.ultra_restricted),
    };
    let result = blacklist::add_comment(&state, session.account, id, &form.comment, flags).await;
    done(&state, &session, shell, result, "Comment added.").await
}
