//! Notifications: the messages Tether sends (Alliance Auth's wording where
//! AA has one) and the live unread count. Storage is in
//! `tether_db::notifications`.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use askama::Template;
use axum::response::sse::Event;
use futures_core::Stream;
use sqlx::postgres::{PgListener, PgPoolOptions};
use tokio::sync::{broadcast, oneshot, watch};
use tokio::time::Instant;

use tether_db::PgPool;
use tether_db::accounts::{AccountId, Lost};
use tether_db::groups::GroupId;
use tether_db::notifications::{Level, notify, notify_once};
use tether_db::settings;

/// "State changed to: {state}" (AA's).
pub async fn state_changed(
    tx: &mut sqlx::PgConnection,
    account: AccountId,
    state: &str,
) -> Result<(), sqlx::Error> {
    notify(
        tx,
        account,
        Level::Info,
        &format!("State changed to: {state}"),
        Some(&format!("Your user's state is now: {state}")),
    )
    .await
}

/// Compliance gained or lost.
pub async fn compliance_changed(
    tx: &mut sqlx::PgConnection,
    account: AccountId,
    compliant: bool,
    state: &str,
) -> Result<(), sqlx::Error> {
    if compliant {
        notify(
            tx,
            account,
            Level::Success,
            "Compliance restored",
            Some(&format!(
                "Every character on your account is registered with the access {state} needs."
            )),
        )
        .await
    } else {
        notify(
            tx,
            account,
            Level::Warning,
            "Compliance lost",
            Some(&format!(
                "Not every character on your account is registered with the access {state} \
                 needs, so some features wait until they are. Use Register Character to fix it."
            )),
        )
        .await
    }
}

/// A leader's decision on a join or leave request (AA's four messages).
pub async fn group_decision(
    tx: &mut sqlx::PgConnection,
    account: AccountId,
    group: &str,
    leave: bool,
    accepted: bool,
) -> Result<(), sqlx::Error> {
    let (level, title, message) = match (leave, accepted) {
        (false, true) => (
            Level::Success,
            "Group Application Accepted",
            format!("Your application to {group} has been accepted."),
        ),
        (false, false) => (
            Level::Danger,
            "Group Application Rejected",
            format!("Your application to {group} has been rejected."),
        ),
        (true, true) => (
            Level::Success,
            "Group Leave Request Accepted",
            format!("Your request to leave {group} has been accepted."),
        ),
        (true, false) => (
            Level::Danger,
            "Group Leave Request Rejected",
            format!("Your request to leave {group} has been rejected."),
        ),
    };
    notify(tx, account, level, title, Some(&message)).await
}

/// A new join or leave request, to the group's leaders, when the setting
/// is on (AA's `GROUPMANAGEMENT_REQUESTS_NOTIFICATION`, off by default).
pub async fn group_request(
    tx: &mut sqlx::PgConnection,
    group: GroupId,
    group_name: &str,
    requester: AccountId,
    leave: bool,
) -> Result<(), sqlx::Error> {
    if !settings::get_bool(&mut *tx, settings::GROUPS_NOTIFY_REQUESTS).await? {
        return Ok(());
    }
    let user = tether_db::accounts::main_name(&mut *tx, requester)
        .await?
        .unwrap_or_else(|| "A pilot".to_owned());
    let (title, message) = if leave {
        (
            format!("Group Management: Leave request for {group_name}"),
            format!("{user} wants to leave {group_name}."),
        )
    } else {
        (
            format!("Group Management: Join request for {group_name}"),
            format!("{user} wants to join {group_name}."),
        )
    };
    for leader in tether_db::groups::leader_accounts(&mut *tx, group).await? {
        if leader != requester {
            // Once while unread: a join-retract loop can't flood them.
            notify_once(tx, leader, Level::Info, &title, Some(&message)).await?;
        }
    }
    Ok(())
}

/// A character that left its account: sold, signed in on another account,
/// or its EVE access gone.
pub async fn character_lost(db: &PgPool, lost: &Lost) -> Result<(), sqlx::Error> {
    let name = &lost.character_name;
    let why = match lost.reason {
        "sold" => format!("{name} now belongs to another EVE account, so it has left yours."),
        "moved" => format!("{name} was signed in to another Tether account, so it moved there."),
        _ => format!(
            "EVE access for {name} has ended, so it has left your account. Add it again with \
             Add Character."
        ),
    };
    let message = if lost.was_main {
        format!("{why} It was your main: choose a new one with Change Main.")
    } else {
        why
    };
    let mut conn = db.acquire().await?;
    notify(
        &mut conn,
        lost.from,
        Level::Warning,
        &format!("Character {name} lost"),
        Some(&message),
    )
    .await
}

/// A Corporation Stats source that stopped working, to its owner (once).
pub async fn corp_source_failed(
    tx: &mut sqlx::PgConnection,
    account: AccountId,
    character: &str,
) -> Result<(), sqlx::Error> {
    notify(
        tx,
        account,
        Level::Warning,
        &format!("Corporation Stats: {character} stopped working"),
        Some(&format!(
            "Tether couldn't read your corporation's member list with {character}. Log in with it \
             again through Register Character, or withdraw it on the Compliance page."
        )),
    )
    .await
}

// ---- the live unread count ------------------------------------------------

/// Live streams one account may hold open at once (tabs, devices); a
/// ninth ends the oldest, which is usually a closed tab the connection
/// hasn't noticed yet.
pub const MAX_STREAMS: usize = 8;
/// Keep-alive comments this often, which also finds closed tabs.
const KEEP_ALIVE: Duration = Duration::from_secs(5);
/// At most one count query per stream this often, however many changes
/// (or forged `NOTIFY`s) arrive.
const MIN_GAP: Duration = Duration::from_secs(1);
/// How long one stream lasts; the browser reconnects (and so signs in
/// again) after it ends.
const STREAM_FOR: Duration = Duration::from_secs(3600);

/// Whose unread count moved, from Postgres (`LISTEN tether_notifications`,
/// fired by triggers), so changes from jobs and the CLI arrive too.
#[derive(Clone)]
pub struct Notices {
    changes: broadcast::Sender<i64>,
    /// Flipped at shutdown so open streams end and HTTP can drain.
    stopping: watch::Sender<bool>,
    open: Arc<Mutex<Open>>,
}

/// Each account's open streams, oldest first, with a way to end each.
#[derive(Default)]
struct Open {
    next_id: u64,
    by_account: HashMap<i64, VecDeque<(u64, oneshot::Sender<()>)>>,
}

impl Notices {
    fn new() -> Self {
        Self {
            changes: broadcast::channel(256).0,
            stopping: watch::channel(false).0,
            open: Arc::default(),
        }
    }

    /// Starts listening in the background on a connection of its own (not
    /// one of the pool's), reconnecting after errors.
    pub fn start(db: PgPool) -> Self {
        let notices = Self::new();
        let tx = notices.changes.clone();
        tokio::spawn(async move {
            let own = PgPoolOptions::new()
                .max_connections(1)
                .connect_lazy_with((*db.connect_options()).clone());
            // Until the app's pool closes (shutdown, or a test's end).
            let closed = db.close_event();
            tokio::pin!(closed);
            loop {
                tokio::select! {
                    result = listen(&own, &tx) => {
                        if let Err(err) = result {
                            tracing::warn!(error = %err, "notifications listener; retrying");
                        }
                    }
                    () = &mut closed => break,
                }
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(5)) => {}
                    () = &mut closed => break,
                }
            }
            own.close().await;
        });
        notices
    }

    /// One that hears nothing (pages still show the count on load).
    pub fn idle() -> Self {
        Self::new()
    }

    /// Ends every open stream (at shutdown).
    pub fn stop(&self) {
        self.stopping.send_replace(true);
    }

    /// A place for one of the account's streams, ending its oldest when
    /// it already has [`MAX_STREAMS`].
    fn claim(&self, account: AccountId) -> (Slot, oneshot::Receiver<()>) {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let id = open.next_id;
        open.next_id += 1;
        let (end, ended) = oneshot::channel();
        let streams = open.by_account.entry(account.0).or_default();
        while streams.len() >= MAX_STREAMS {
            if let Some((_, oldest)) = streams.pop_front() {
                // It may have ended already.
                let _ = oldest.send(());
            }
        }
        streams.push_back((id, end));
        let slot = Slot {
            open: self.open.clone(),
            account: account.0,
            id,
        };
        (slot, ended)
    }
}

/// Frees a stream's place when it ends.
struct Slot {
    open: Arc<Mutex<Open>>,
    account: i64,
    id: u64,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(streams) = open.by_account.get_mut(&self.account) {
            streams.retain(|(id, _)| *id != self.id);
            if streams.is_empty() {
                open.by_account.remove(&self.account);
            }
        }
    }
}

async fn listen(db: &PgPool, tx: &broadcast::Sender<i64>) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(db).await?;
    listener.listen(tether_db::notifications::CHANNEL).await?;
    loop {
        let notice = listener.recv().await?;
        if let Ok(account) = notice.payload().parse::<i64>() {
            // No open pages is fine.
            let _ = tx.send(account);
        }
    }
}

#[derive(Template)]
#[template(path = "notification_bell_fragment.html")]
struct Bell {
    shell: BellCount,
}

struct BellCount {
    unread: i64,
}

struct Watch {
    db: PgPool,
    rx: broadcast::Receiver<i64>,
    stopping: watch::Receiver<bool>,
    account: AccountId,
    /// The session's token hash: the stream ends once it's no longer valid
    /// (logged out, expired, deactivated).
    session: Vec<u8>,
    last: Option<i64>,
    until: Instant,
    /// Fires when a newer stream of the account's takes this one's place.
    evicted: oneshot::Receiver<()>,
    _slot: Slot,
}

type Step = Pin<Box<dyn Future<Output = Option<(Event, Watch)>> + Send>>;

/// Server-sent `unread` events carrying the top bar's bell (its HTML),
/// sent at once and again whenever the account's unread count changes.
pub struct UnreadStream {
    step: Option<Step>,
}

impl UnreadStream {
    pub fn new(db: PgPool, notices: &Notices, account: AccountId, session_hash: Vec<u8>) -> Self {
        let (slot, evicted) = notices.claim(account);
        let watch = Watch {
            db,
            rx: notices.changes.subscribe(),
            stopping: notices.stopping.subscribe(),
            account,
            session: session_hash,
            last: None,
            until: Instant::now() + STREAM_FOR,
            evicted,
            _slot: slot,
        };
        Self {
            step: Some(Box::pin(next(watch))),
        }
    }

    /// Keep-alive for the stream's response.
    pub fn keep_alive() -> axum::response::sse::KeepAlive {
        axum::response::sse::KeepAlive::new().interval(KEEP_ALIVE)
    }
}

impl Stream for UnreadStream {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(step) = self.step.as_mut() else {
            return Poll::Ready(None);
        };
        match step.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                self.step = None;
                Poll::Ready(None)
            }
            Poll::Ready(Some((event, watch))) => {
                self.step = Some(Box::pin(next(watch)));
                Poll::Ready(Some(Ok(event)))
            }
        }
    }
}

async fn next(mut w: Watch) -> Option<(Event, Watch)> {
    if *w.stopping.borrow() {
        return None;
    }
    loop {
        if w.last.is_some() {
            tokio::select! {
                got = w.rx.recv() => match got {
                    Ok(account) if account == w.account.0 => {}
                    Ok(_) => continue,
                    // Missed some: check anyway.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return None,
                },
                _ = w.stopping.changed() => return None,
                _ = &mut w.evicted => return None,
                () = tokio::time::sleep_until(w.until) => return None,
            }
            // Let a burst of changes settle into one count.
            tokio::select! {
                () = tokio::time::sleep(MIN_GAP) => {}
                _ = w.stopping.changed() => return None,
                _ = &mut w.evicted => return None,
            }
            loop {
                match w.rx.try_recv() {
                    Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(broadcast::error::TryRecvError::Closed) => return None,
                    Err(broadcast::error::TryRecvError::Empty) => break,
                }
            }
        }
        match tether_db::auth::session_valid(&w.db, &w.session).await {
            Ok(true) => {}
            Ok(false) => return None,
            Err(err) => {
                tracing::warn!(error = %err, "checking a stream's session");
                return None;
            }
        }
        let unread = match tether_db::notifications::unread(&w.db, w.account).await {
            Ok(unread) => unread,
            Err(err) => {
                tracing::warn!(error = %err, "unread count");
                return None;
            }
        };
        if w.last == Some(unread) {
            continue;
        }
        w.last = Some(unread);
        let html = match (Bell {
            shell: BellCount { unread },
        })
        .render()
        {
            Ok(html) => html,
            Err(err) => {
                tracing::warn!(error = %err, "rendering the bell");
                return None;
            }
        };
        // SSE can't carry carriage returns (axum panics on them).
        let event = Event::default()
            .event("unread")
            .data(html.replace('\r', ""));
        return Some((event, w));
    }
}
