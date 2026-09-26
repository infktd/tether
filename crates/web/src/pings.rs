//! Fleet pings (F13): a message to a Discord channel an admin chose,
//! pinging @here, @everyone or a Tether-managed role. Only that target can
//! ping: Discord ignores user and role mentions typed into the message
//! (`allowed_mentions`), and typed @everyone and @here are defused in the
//! text, since Discord allows both or neither.
//!
//! A ping is recorded together with a delayed retry job, then sent straight
//! away; a successful send makes the job a no-op. A ping more than 15
//! minutes old is no use to anyone and is dropped instead of sent late.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use tether_core::crypto::EncryptionKey;
use tether_db::accounts::{self, AccountId};
use tether_db::audit::{self, Actor};
use tether_db::ping_options::{self, Kind, PingOption};
use tether_db::pings::{self as db, Details, NewPing, PingChannel, Target};
use tether_db::{PgPool, discord as discord_db};
use tether_discord::store;
use tether_discord::{Discord, DiscordError, Embed, Mention};
use tether_jobs::{JobError, NewJob, Registry};

use crate::AppState;
use crate::discord::{config, discord_error};
use crate::error::AppError;

pub const PING_JOB: &str = "discord.ping";
pub const MAX_MESSAGE: usize = 1500;
/// Per account.
pub const PINGS_PER_WINDOW: i64 = 5;
pub const WINDOW: Duration = Duration::from_secs(10 * 60);
/// Retried pings older than this are dropped.
pub const STALE_AFTER: Duration = Duration::from_secs(15 * 60);

/// Makes a Discord channel a ping channel. The name comes from Discord.
pub async fn add_channel(
    state: &AppState,
    actor: AccountId,
    channel_id: &str,
) -> Result<(), AppError> {
    let id: u64 = channel_id
        .trim()
        .parse()
        .map_err(|_| AppError::bad_request("Choose a channel."))?;
    let config = config(state).await?;
    let channel = state
        .discord
        .text_channels(&config)
        .await
        .map_err(discord_error)?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| AppError::not_found("That isn't a text channel on the server."))?;
    let stored = i64::try_from(channel.id).map_err(AppError::internal)?;
    let guild = i64::try_from(config.guild_id).map_err(AppError::internal)?;
    let mut tx = state.db.begin().await?;
    if !db::add_channel(&mut *tx, stored, guild, &channel.name).await? {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "That's already a ping channel.",
        ));
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "ping.channel.add",
        Some(&format!("channel:{stored}")),
        json!({ "name": channel.name }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn remove_channel(
    state: &AppState,
    actor: AccountId,
    channel_id: i64,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    let channel = db::remove_channel(&mut *tx, channel_id)
        .await?
        .ok_or_else(|| AppError::not_found("No such ping channel."))?;
    let limits = ping_options::clear(&mut *tx, &channel_item(channel_id)).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "ping.channel.remove",
        Some(&format!("channel:{channel_id}")),
        json!({ "name": channel.name, "limits_removed": limits_json(&limits) }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Who a ping may target: nobody, @here, @everyone, or a role Tether
/// manages (the ones mapped to states and groups).
pub async fn targets(state: &AppState) -> Result<Vec<Target>, AppError> {
    let mut targets = vec![Target::None, Target::Here, Target::Everyone];
    let mut seen = std::collections::HashSet::new();
    for mapping in discord_db::mappings(&state.db).await? {
        if seen.insert(mapping.role_id) {
            targets.push(Target::Role {
                id: mapping.role_id,
                name: mapping.role_name,
            });
        }
    }
    Ok(targets)
}

/// A `<select>` value: `none`, `here`, `everyone` or `role:<id>`.
pub fn target_value(target: &Target) -> String {
    match target {
        Target::None => "none".to_owned(),
        Target::Here => "here".to_owned(),
        Target::Everyone => "everyone".to_owned(),
        Target::Role { id, .. } => format!("role:{id}"),
    }
}

/// What one account may use: the channels, targets, fleet types and
/// doctrines not limited away from it (the owner may use everything), and
/// every formup location and comms channel.
#[derive(Debug, Clone, Default)]
pub struct Offer {
    pub channels: Vec<PingChannel>,
    pub targets: Vec<Target>,
    pub fleet_types: Vec<PingOption>,
    pub doctrines: Vec<PingOption>,
    pub formups: Vec<PingOption>,
    pub comms: Vec<PingOption>,
    /// Configured doctrines this account may not use: their names can't be
    /// typed in either.
    pub closed_doctrines: Vec<String>,
}

/// A target's key in `core.ping_restrictions`; `None` pings nobody and is
/// never limited.
pub fn target_item(target: &Target) -> Option<String> {
    match target {
        Target::None => None,
        Target::Here => Some("here".to_owned()),
        Target::Everyone => Some("everyone".to_owned()),
        Target::Role { id, .. } => Some(format!("role:{id}")),
    }
}

pub fn channel_item(channel_id: i64) -> String {
    format!("channel:{channel_id}")
}

pub async fn offer(state: &AppState, account: AccountId) -> Result<Offer, AppError> {
    let channels = match config(state).await {
        Ok(config) => channels_for(state, &config).await?,
        Err(_) => Vec::new(),
    };
    let owner = accounts::get(&state.db, account)
        .await?
        .is_some_and(|a| a.is_owner);
    let access = ping_options::access(&state.db, account).await?;
    let may = |item: &str| owner || access.may_use(item);
    let mass =
        tether_db::settings::get_bool_or(&state.db, tether_db::settings::PINGS_MASS_MENTIONS, true)
            .await?;
    let mut offer = Offer {
        channels: channels
            .into_iter()
            .filter(|c| may(&channel_item(c.channel_id)))
            .collect(),
        targets: targets(state)
            .await?
            .into_iter()
            .filter(|t| mass || !matches!(t, Target::Here | Target::Everyone))
            .filter(|t| target_item(t).is_none_or(|item| may(&item)))
            .collect(),
        ..Offer::default()
    };
    for option in ping_options::list(&state.db).await? {
        let open = may(&option.item());
        match option.kind {
            Kind::FleetType if open => offer.fleet_types.push(option),
            Kind::Doctrine if open => offer.doctrines.push(option),
            Kind::Doctrine => offer.closed_doctrines.push(option.name),
            Kind::Formup => offer.formups.push(option),
            Kind::Comms => offer.comms.push(option),
            Kind::FleetType => {}
        }
    }
    Ok(offer)
}

/// A ping as typed on the form (aa-fleetpings' fields).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PingForm {
    #[serde(default)]
    pub channel_id: String,
    #[serde(default)]
    pub target: String,
    /// Additional information.
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub pre_ping: Option<String>,
    #[serde(default)]
    pub fleet_type: String,
    #[serde(default)]
    pub fc_name: String,
    #[serde(default)]
    pub fleet_name: String,
    #[serde(default)]
    pub formup_location: String,
    #[serde(default)]
    pub formup_now: Option<String>,
    /// `YYYY-MM-DDTHH:MM`, EVE time.
    #[serde(default)]
    pub formup_time: String,
    #[serde(default)]
    pub comms: String,
    #[serde(default)]
    pub doctrine: String,
    /// `yes`, `no`, or empty for not said.
    #[serde(default)]
    pub srp: String,
}

/// Longest detail (a name, a place, a channel).
pub const MAX_FIELD: usize = 100;

fn field(label: &str, value: &str) -> Result<Option<String>, AppError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > MAX_FIELD || value.chars().any(invisible) {
        return Err(AppError::bad_request(format!(
            "{label} is at most {MAX_FIELD} characters, on one line."
        )));
    }
    Ok(Some(value.to_owned()))
}

/// Control, format and separator characters: invisible, or reordering the
/// text around them (so "Sup\u{200B}ers" can't pass for "Supers").
fn invisible(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

/// Checks the form's details against what the account may use.
pub fn details(form: &PingForm, offer: &Offer) -> Result<Details, AppError> {
    let fleet_type = match field("The fleet type", &form.fleet_type)? {
        None => None,
        Some(name) => Some(
            offer
                .fleet_types
                .iter()
                .find(|t| t.name == name)
                .ok_or_else(|| AppError::bad_request("Choose one of the fleet types."))?,
        ),
    };
    let doctrine = field("The doctrine", &form.doctrine)?;
    if let Some(name) = &doctrine
        && offer
            .closed_doctrines
            .iter()
            .any(|d| d.eq_ignore_ascii_case(name))
    {
        return Err(AppError::bad_request("That doctrine isn't open to you."));
    }
    let doctrine_link = doctrine.as_ref().and_then(|name| {
        offer
            .doctrines
            .iter()
            .find(|d| d.name.eq_ignore_ascii_case(name))
            .and_then(|d| d.link.clone())
    });
    let formup_now = form.formup_now.is_some();
    let formup_time = if formup_now || form.formup_time.trim().is_empty() {
        None
    } else {
        let at = chrono::NaiveDateTime::parse_from_str(form.formup_time.trim(), "%Y-%m-%dT%H:%M")
            .map_err(|_| AppError::bad_request("Give the formup time as a date and time (EVE)."))?
            .and_utc();
        let now = chrono::Utc::now();
        if at < now - chrono::Duration::days(1) || at > now + chrono::Duration::days(366) {
            return Err(AppError::bad_request(
                "The formup time is at most a day ago and within a year.",
            ));
        }
        Some(at)
    };
    Ok(Details {
        pre_ping: form.pre_ping.is_some(),
        embed_color: fleet_type.and_then(|t| t.color.clone()),
        fleet_type: fleet_type.map(|t| t.name.clone()),
        fc_name: field("The FC", &form.fc_name)?,
        fleet_name: field("The fleet name", &form.fleet_name)?,
        formup_location: field("The formup location", &form.formup_location)?,
        formup_time,
        formup_now,
        comms: field("Comms", &form.comms)?,
        doctrine,
        doctrine_link,
        srp: match form.srp.as_str() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        },
    })
}

/// Records the ping and sends it. Returns its id.
pub async fn send(state: &AppState, actor: AccountId, form: &PingForm) -> Result<i64, AppError> {
    let offer = offer(state, actor).await?;
    let details = details(form, &offer)?;
    let message = form.message.trim();
    if message.is_empty() && details.is_empty() {
        return Err(AppError::bad_request(
            "Fill in the fleet details or write a message.",
        ));
    }
    if message.chars().count() > MAX_MESSAGE {
        return Err(AppError::bad_request(format!(
            "Pings are at most {MAX_MESSAGE} characters."
        )));
    }
    let channel_id: i64 = form
        .channel_id
        .trim()
        .parse()
        .map_err(|_| AppError::bad_request("Choose a channel."))?;
    let channel = offer
        .channels
        .iter()
        .find(|c| c.channel_id == channel_id)
        .cloned()
        .ok_or_else(|| AppError::bad_request("That isn't a ping channel you may use."))?;
    let target = offer
        .targets
        .iter()
        .find(|t| target_value(t) == form.target)
        .cloned()
        .ok_or_else(|| AppError::bad_request("Choose who to ping."))?;
    let sender = accounts::get(&state.db, actor)
        .await?
        .ok_or_else(AppError::unauthorized)?
        .main
        .ok_or_else(|| AppError::bad_request("Choose a main character first (Change Main)."))?
        .name;
    let nonce = format!(
        "tp-{}",
        &tether_core::new_token()
            .map_err(AppError::internal)?
            .expose()[..20]
    );

    let mut tx = state.db.begin().await?;
    // Counted under the sender's row lock, so parallel sends can't all
    // slip under the limit.
    db::lock_sender(&mut tx, actor).await?;
    if db::sent_since(&mut *tx, actor, WINDOW.as_secs_f64()).await? >= PINGS_PER_WINDOW {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "That's {PINGS_PER_WINDOW} pings in the last {} minutes. Wait a little.",
                WINDOW.as_secs() / 60
            ),
        ));
    }
    let id = db::insert(
        &mut *tx,
        NewPing {
            account: actor,
            sender_name: &sender,
            channel: &channel,
            target: &target,
            message,
            details: &details,
            nonce: &nonce,
        },
    )
    .await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "ping.send",
        Some(&format!("ping:{id}")),
        json!({
            "channel": channel.name,
            "target": target_value(&target),
            "fleet_type": details.fleet_type,
            "length": message.chars().count(),
        }),
    )
    .await?;
    // The retry, queued with the ping so it can't be lost; sending now
    // makes it a no-op. Its attempts outlast STALE_AFTER, so a ping that
    // never goes out ends up marked failed.
    tether_jobs::enqueue(
        &mut *tx,
        NewJob::new(PING_JOB, json!({ "ping_id": id }))
            .max_attempts(10)
            .run_at(chrono::Utc::now() + chrono::Duration::seconds(60)),
    )
    .await?;
    tx.commit().await?;

    match deliver(&state.db, &state.key, &state.discord, id).await {
        Ok(()) => {}
        Err(JobError::Retry(reason)) => {
            tracing::info!(ping = id, reason, "fleet ping will be retried");
        }
        Err(JobError::Permanent(reason)) => {
            tracing::warn!(ping = id, reason, "fleet ping failed");
        }
        // deliver never defers; the retry job would pick it up anyway.
        Err(JobError::Defer(_)) => {}
    }
    Ok(id)
}

// ---- settings (admin.discord) ------------------------------------------

/// Adds a fleet type (with its embed colour), doctrine (with a link),
/// formup location or comms channel to what the form offers.
pub async fn add_option(
    state: &AppState,
    actor: AccountId,
    kind: &str,
    name: &str,
    link: &str,
    color: &str,
) -> Result<(), AppError> {
    let kind = Kind::parse(kind).ok_or_else(|| AppError::bad_request("Choose what to add."))?;
    let name = field("The name", name)?.ok_or_else(|| AppError::bad_request("Give it a name."))?;
    let link = match (kind, link.trim()) {
        (Kind::Doctrine, "") | (_, "") => None,
        (Kind::Doctrine, link) => {
            // Checked after parsing, which percent-encodes: the link must
            // fit Discord's field alongside the doctrine's name.
            let url = reqwest::Url::parse(link)
                .ok()
                .filter(|u| {
                    u.scheme() == "https" && u.host_str().is_some() && u.as_str().len() <= 500
                })
                .ok_or_else(|| {
                    AppError::bad_request("A doctrine link must be an https:// address.")
                })?;
            Some(url.to_string())
        }
        _ => None,
    };
    let color = match (kind, color.trim()) {
        (Kind::FleetType, "") | (_, "") => None,
        (Kind::FleetType, color) => {
            let color = color.to_ascii_lowercase();
            let valid = color.len() == 7
                && color.starts_with('#')
                && color[1..].chars().all(|c| c.is_ascii_hexdigit());
            if !valid {
                return Err(AppError::bad_request("A colour is # and six hex digits."));
            }
            Some(color)
        }
        _ => None,
    };
    let mut tx = state.db.begin().await?;
    let id = ping_options::add(&mut *tx, kind, &name, link.as_deref(), color.as_deref())
        .await?
        .ok_or_else(|| {
            AppError::new(StatusCode::CONFLICT, "There's already one with that name.")
        })?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "ping.option.add",
        Some(&format!("ping_option:{id}")),
        json!({ "kind": kind.as_str(), "name": name, "link": link, "color": color }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn delete_option(state: &AppState, actor: AccountId, id: i64) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    let (name, limits) = ping_options::delete(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("No such option."))?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "ping.option.delete",
        Some(&format!("ping_option:{id}")),
        json!({ "name": name, "limits_removed": limits_json(&limits) }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The canonical key for `item`, if it names something that exists: a
/// ping channel, a target or a fleet type or doctrine.
async fn canonical_item(state: &AppState, item: &str) -> Result<Option<String>, AppError> {
    if item == "here" || item == "everyone" {
        return Ok(Some(item.to_owned()));
    }
    let id = |prefix: &str| {
        item.strip_prefix(prefix)
            .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|id| id.parse::<i64>().ok())
    };
    if let Some(id) = id("channel:") {
        let config = config(state).await?;
        let exists = channels_for(state, &config)
            .await?
            .iter()
            .any(|c| c.channel_id == id);
        return Ok(exists.then(|| channel_item(id)));
    }
    if let Some(id) = id("role:") {
        let exists = targets(state)
            .await?
            .iter()
            .any(|t| matches!(t, Target::Role { id: r, .. } if *r == id));
        return Ok(exists.then(|| format!("role:{id}")));
    }
    if let Some(id) = id("option:") {
        let exists = ping_options::list(&state.db)
            .await?
            .iter()
            .any(|o| o.id == id && matches!(o.kind, Kind::FleetType | Kind::Doctrine));
        return Ok(exists.then(|| format!("option:{id}")));
    }
    Ok(None)
}

fn limits_json(limits: &[ping_options::Removed]) -> serde_json::Value {
    limits
        .iter()
        .map(|l| json!({ "state_id": l.state_id, "group_id": l.group_id }))
        .collect()
}

/// Limits a channel, target, fleet type or doctrine to a state or group
/// (in addition to any already listed).
pub async fn restrict(
    state: &AppState,
    actor: AccountId,
    item: &str,
    grantee: tether_db::permissions::Grantee,
) -> Result<(), AppError> {
    let item = canonical_item(state, item)
        .await?
        .ok_or_else(|| AppError::not_found("Nothing to limit there."))?;
    let (state_id, group_id) = match grantee {
        tether_db::permissions::Grantee::State(s) => (Some(s.0), None),
        tether_db::permissions::Grantee::Group(g) => (None, Some(g.0)),
    };
    let mut tx = state.db.begin().await?;
    match ping_options::restrict(&mut *tx, &item, state_id, group_id).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                "It's already limited to that.",
            ));
        }
        Err(err) if crate::error::is_foreign_key_violation(&err) => {
            return Err(AppError::not_found("No such state or group."));
        }
        Err(err) => return Err(err.into()),
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "ping.restrict",
        Some(&item),
        json!({ "state_id": state_id, "group_id": group_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn unrestrict(state: &AppState, actor: AccountId, id: i64) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    let removed = ping_options::unrestrict(&mut *tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("No such limit."))?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "ping.unrestrict",
        Some(&removed.item),
        json!({ "state_id": removed.state_id, "group_id": removed.group_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Turns @here and @everyone on or off for every ping.
pub async fn set_mass_mentions(
    state: &AppState,
    actor: AccountId,
    on: bool,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    tether_db::settings::set(
        &mut *tx,
        tether_db::settings::PINGS_MASS_MENTIONS,
        serde_json::Value::Bool(on),
    )
    .await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "ping.settings",
        None,
        json!({ "mass_mentions": on }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The ping channels on the configured server.
pub async fn channels_for(
    state: &AppState,
    config: &tether_discord::DiscordConfig,
) -> Result<Vec<PingChannel>, AppError> {
    let guild = i64::try_from(config.guild_id).map_err(AppError::internal)?;
    Ok(db::channels(&state.db, guild).await?)
}

/// Breaks @everyone and @here typed into text with a zero-width space:
/// `allowed_mentions` can't allow @here but not @everyone.
pub(crate) fn defuse(text: &str) -> String {
    text.replace("@everyone", "@\u{200B}everyone")
        .replace("@here", "@\u{200B}here")
}

/// The message as posted: the mention and a headline, or, for a plain
/// ping, the mention, the message and who sent it.
pub fn content(ping: &db::Ping) -> String {
    let mention = mention(&ping.target).prefix();
    let mut text = String::new();
    if !mention.is_empty() {
        text.push_str(&mention);
        text.push('\n');
    }
    if ping.details.is_empty() {
        text.push_str(&defuse(&ping.message));
        text.push_str("\n— ");
        text.push_str(&defuse(&ping.sender_name));
    } else {
        text.push_str("**");
        text.push_str(&defuse(&headline(&ping.details)));
        text.push_str("**");
    }
    text
}

/// "Pre-Ping: Roaming Fleet", "Home Defense Fleet", "Fleet".
pub fn headline(details: &Details) -> String {
    let fleet = details
        .fleet_type
        .as_deref()
        .map_or_else(|| "Fleet".to_owned(), |t| format!("{t} Fleet"));
    if details.pre_ping {
        format!("Pre-Ping: {fleet}")
    } else {
        fleet
    }
}

fn formup_time(details: &Details) -> Option<String> {
    if details.formup_now {
        Some("NOW".to_owned())
    } else {
        details
            .formup_time
            .map(|t| t.format("%Y-%m-%d %H:%M EVE").to_string())
    }
}

/// The details as `(label, value)`, in aa-fleetpings' order.
fn detail_lines(details: &Details) -> Vec<(&'static str, String)> {
    let mut lines = Vec::new();
    let mut add = |label, value: Option<String>| {
        if let Some(value) = value {
            lines.push((label, value));
        }
    };
    add("FC", details.fc_name.clone());
    add("Fleet Name", details.fleet_name.clone());
    add("Formup Location", details.formup_location.clone());
    add("Formup Time", formup_time(details));
    add("Comms", details.comms.clone());
    add("Doctrine", details.doctrine.clone());
    add(
        "SRP",
        details
            .srp
            .map(|srp| if srp { "Yes" } else { "No" }.to_owned()),
    );
    lines
}

/// The card under a detailed ping, coloured by its fleet type.
pub fn embed(ping: &db::Ping) -> Option<Embed> {
    if ping.details.is_empty() {
        return None;
    }
    let d = &ping.details;
    let fields = detail_lines(d)
        .into_iter()
        .map(|(label, value)| {
            let value = match (label, &d.doctrine_link) {
                // Discord renders a markdown link in a field.
                ("Doctrine", Some(link)) => {
                    format!("[{}]({link})", defuse(&value).replace(['[', ']'], ""))
                }
                _ => defuse(&value),
            };
            (label.to_owned(), value)
        })
        .collect();
    let formup_at = if d.formup_now {
        None
    } else {
        d.formup_time.map(|t| t.timestamp())
    };
    Some(Embed {
        title: defuse(d.fleet_name.as_deref().unwrap_or(&headline(d))),
        description: {
            let mut text = defuse(&ping.message);
            // Each reader's own clock, as Discord shows timestamps.
            if let Some(at) = formup_at {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(&format!("Formup <t:{at}:R>"));
            }
            (!text.is_empty()).then_some(text)
        },
        color: d
            .embed_color
            .as_deref()
            .and_then(|c| u32::from_str_radix(c.trim_start_matches('#'), 16).ok()),
        fields,
        footer: Some(format!("Sent by {} via Tether", defuse(&ping.sender_name))),
    })
}

/// The ping as plain text, to paste into EVE or another chat
/// (aa-fleetpings' copy-paste text).
pub fn copy_text(details: &Details, message: &str) -> String {
    let mut text = headline(details);
    for (label, value) in detail_lines(details) {
        text.push('\n');
        text.push_str(label);
        text.push_str(": ");
        text.push_str(&value);
    }
    let message = message.trim();
    if !message.is_empty() {
        text.push_str("\n\n");
        text.push_str(message);
    }
    text
}

fn mention(target: &Target) -> Mention {
    match target {
        Target::None => Mention::None,
        Target::Here => Mention::Here,
        Target::Everyone => Mention::Everyone,
        Target::Role { id, .. } => u64::try_from(*id).map_or(Mention::None, Mention::Role),
    }
}

/// Sends a recorded ping, once. Transient failures are `Retry`; anything
/// else (or a ping gone stale) is recorded as failed.
pub async fn deliver(
    db_pool: &PgPool,
    key: &EncryptionKey,
    discord: &Discord,
    ping_id: i64,
) -> Result<(), JobError> {
    let Some(ping) = db::get(db_pool, ping_id, STALE_AFTER)
        .await
        .map_err(JobError::retry)?
    else {
        return Ok(());
    };
    if ping.sent_at.is_some() || ping.failed_at.is_some() {
        return Ok(());
    }
    let fail = |reason: String| async move {
        db::mark_error(db_pool, ping_id, &reason, true)
            .await
            .map_err(JobError::retry)?;
        Err::<(), _>(JobError::permanent(reason))
    };
    if ping.stale {
        return fail("Discord was unavailable for too long; not sent".to_owned()).await;
    }
    let Some(config) = store::load(db_pool, key).await.map_err(JobError::retry)? else {
        return fail("Discord isn't set up".to_owned()).await;
    };
    // Removed as a ping channel, or Tether moved to another server, since
    // it was queued.
    let guild = i64::try_from(config.guild_id).map_err(JobError::permanent)?;
    if !db::is_channel(db_pool, ping.channel_id, guild)
        .await
        .map_err(JobError::retry)?
    {
        return fail("That channel is no longer a ping channel; not sent".to_owned()).await;
    }
    let channel = u64::try_from(ping.channel_id).map_err(JobError::permanent)?;
    let nonce = ping.nonce.clone();
    match discord
        .send_message(
            &config,
            channel,
            &content(&ping),
            embed(&ping).as_ref(),
            mention(&ping.target),
            &nonce,
        )
        .await
    {
        Ok(message_id) => {
            let message_id = i64::try_from(message_id).map_err(JobError::permanent)?;
            db::mark_sent(db_pool, ping_id, message_id)
                .await
                .map_err(JobError::retry)?;
            tracing::info!(
                ping = ping_id,
                channel = ping.channel_name,
                "fleet ping sent"
            );
            Ok(())
        }
        Err(err) if err.is_transient() => {
            db::mark_error(db_pool, ping_id, &err.to_string(), false)
                .await
                .map_err(JobError::retry)?;
            Err(JobError::retry(err))
        }
        Err(err) => fail(explain(&err)).await,
    }
}

fn explain(err: &DiscordError) -> String {
    match err.code() {
        Some(tether_discord::codes::MISSING_ACCESS | tether_discord::codes::MISSING_PERMISSIONS) => {
            "The bot can't post in that channel: give it View Channel and Send Messages there (and Mention Everyone for @everyone)".to_owned()
        }
        Some(10003) => "That channel no longer exists".to_owned(),
        _ => err.to_string(),
    }
}

#[derive(Debug, Deserialize)]
struct PingJob {
    ping_id: i64,
}

pub fn register_jobs(
    registry: &mut Registry,
    db: PgPool,
    key: EncryptionKey,
    discord: Arc<Discord>,
) {
    registry.register(PING_JOB, move |job| {
        let (db, key, discord) = (db.clone(), key.clone(), discord.clone());
        async move {
            let payload: PingJob =
                serde_json::from_value(job.payload).map_err(JobError::permanent)?;
            deliver(&db, &key, &discord, payload.ping_id).await
        }
    });
}

/// For the admin page: ping channels and the server's text channels not
/// yet chosen.
pub async fn channel_options(
    state: &AppState,
) -> Result<(Vec<PingChannel>, Vec<tether_discord::TextChannel>), AppError> {
    let config = match config(state).await {
        Ok(config) => config,
        Err(_) => return Ok((Vec::new(), Vec::new())),
    };
    let chosen = channels_for(state, &config).await?;
    let available = match state.discord.text_channels(&config).await {
        Ok(channels) => channels
            .into_iter()
            .filter(|c| {
                !chosen
                    .iter()
                    .any(|p| i64::try_from(c.id) == Ok(p.channel_id))
            })
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err, "listing Discord channels failed");
            Vec::new()
        }
    };
    Ok((chosen, available))
}
