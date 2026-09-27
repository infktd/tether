//! Mail, on its own pages (`mail/{character}` and
//! `mail/{character}/{mail}`), whose every view Tether records in its
//! audit log (`audit = true` in plugin.toml): of every character whose
//! sheet the viewer may open, as in aa-memberaudit (mail is part of the
//! sheet).

use tether_plugin_sdk::storage::Value as Db;
use tether_plugin_sdk::{Card, Column, Page, PageError, Section, Table, Tone, Value, badge, link};

use crate::access::Access;
use crate::pages::{entity, named};
use crate::sheet::{Freshness, sheet_page, subject};
use crate::{boolean, int, query, text, time_or_blank, with_rows};

/// Mails listed under All, and under each label.
const ALL_ROWS: i64 = 200;
const LABEL_ROWS: i64 = 100;
/// Labels shown as tabs (the host allows 10 tabs: All, these, and lists).
const MAX_LABELS: usize = 8;
/// Longest paragraph the host takes is 2 KiB.
const PARAGRAPH: usize = 1800;

pub(crate) fn render(access: &Access, id: i64, rest: &[&str]) -> Result<Page, PageError> {
    let who = subject(access, id)?;
    if !who.may_read_mail {
        return Err(PageError::NotFound);
    }
    match rest {
        [] => list(&who),
        [mail] => {
            let mail: i64 = mail.parse().map_err(|_| PageError::NotFound)?;
            one(&who, mail)
        }
        _ => Err(PageError::NotFound),
    }
}

/// Columns: mail id, sent, from (id, name, category), subject, read.
fn select(filter: &str) -> String {
    format!(
        "SELECT m.mail_id, m.at, m.from_id, {from}, m.subject, m.is_read, \
                (SELECT name FROM mailing_lists l WHERE l.character_id = m.character_id \
                   AND l.mailing_list_id = m.from_id) \
         FROM mails m WHERE m.character_id = $1 {filter} ORDER BY m.at DESC LIMIT $2",
        from = named("m.from_id")
    )
}

fn table(id: i64, rows: &[Vec<Db>]) -> Table {
    with_rows(
        Table::new(vec![
            Column::numeric("Sent"),
            Column::text("From"),
            Column::text("Subject"),
            Column::text(""),
        ])
        .empty("No mail here."),
        rows.iter().map(|r| {
            let mail = int(r, 0);
            let subject = text(r, 5);
            let subject = if subject.is_empty() {
                "(no subject)".to_owned()
            } else {
                subject
            };
            vec![
                time_or_blank(r, 1),
                sender(r, 2),
                link(subject, format!("mail/{id}/{mail}")).into(),
                if boolean(r, 6) {
                    "".into()
                } else {
                    badge("Unread", Tone::Neutral).into()
                },
            ]
        }),
    )
}

/// The sender at `i` (id, name, category), or its mailing list's name.
fn sender(r: &[Db], i: usize) -> Value {
    let list = text(r, i + 5);
    if list.is_empty() {
        entity(int(r, i), text(r, i + 1), &text(r, i + 2))
    } else {
        format!("{list} (mailing list)").into()
    }
}

fn list(who: &crate::sheet::Subject) -> Result<Page, PageError> {
    let id = who.id;
    let fresh = Freshness::of(id)?;
    let all = query(&select(""), &[id.into(), ALL_ROWS.into()])?;
    let labels = query(
        "SELECT label_id, name, unread FROM mail_labels WHERE character_id = $1 ORDER BY label_id",
        &[id.into()],
    )?;
    let lists = query(
        "SELECT mailing_list_id, name FROM mailing_lists WHERE character_id = $1 ORDER BY name",
        &[id.into()],
    )?;
    let mut page = sheet_page(who, "Mail").tab(
        "All",
        vec![Section::Table(table(id, &all)), fresh.line(&["mail"])],
    );
    for label in labels.iter().take(MAX_LABELS) {
        let label_id = int(label, 0);
        let rows = query(
            &select("AND m.labels @> jsonb_build_array($3::bigint)"),
            &[id.into(), LABEL_ROWS.into(), label_id.into()],
        )?;
        let unread = int(label, 2);
        let name = if unread > 0 {
            format!("{} ({unread})", text(label, 1))
        } else {
            text(label, 1)
        };
        page = page.tab(name, vec![Section::Table(table(id, &rows))]);
    }
    if !lists.is_empty() {
        page = page.tab(
            "Mailing lists",
            vec![Section::Table(with_rows(
                Table::new(vec![Column::text("Mailing list")]),
                lists.iter().map(|r| vec![text(r, 1).into()]),
            ))],
        );
    }
    Ok(page)
}

fn one(who: &crate::sheet::Subject, mail: i64) -> Result<Page, PageError> {
    let id = who.id;
    let rows = query(
        &format!(
            "SELECT m.subject, m.at, m.from_id, {from}, m.body, \
                    (SELECT name FROM mailing_lists l WHERE l.character_id = m.character_id \
                       AND l.mailing_list_id = m.from_id), \
                    coalesce((SELECT string_agg(coalesce( \
                        (SELECT name FROM names n WHERE n.id = (r->>'recipient_id')::bigint), \
                        (SELECT name FROM mailing_lists l WHERE l.character_id = m.character_id \
                           AND l.mailing_list_id = (r->>'recipient_id')::bigint), \
                        r->>'recipient_id'), ', ') \
                      FROM jsonb_array_elements(m.recipients) AS r), ''), \
                    coalesce((SELECT string_agg(l.name, ', ') FROM mail_labels l \
                      WHERE l.character_id = m.character_id AND m.labels @> jsonb_build_array(l.label_id)), '') \
             FROM mails m WHERE m.character_id = $1 AND m.mail_id = $2",
            from = named("m.from_id")
        ),
        &[id.into(), mail.into()],
    )?;
    let m = rows.first().ok_or(PageError::NotFound)?;
    let subject = text(m, 0);
    let list = text(m, 6);
    let from = if list.is_empty() {
        entity(int(m, 2), text(m, 3), &text(m, 4))
    } else {
        format!("{list} (mailing list)").into()
    };
    let mut page = sheet_page(who, "Mail").card(
        Card::new(if subject.is_empty() {
            "(no subject)".to_owned()
        } else {
            subject
        })
        .field("From", from)
        .field("To", text(m, 7))
        .field("Sent", time_or_blank(m, 1))
        .field("Labels", text(m, 8))
        .field("Back", link("All mail", format!("mail/{id}"))),
    );
    match m.get(5).and_then(Db::as_text) {
        Some(body) if !body.is_empty() => {
            for paragraph in paragraphs(body).into_iter().take(30) {
                page = page.text(paragraph);
            }
        }
        Some(_) => page = page.text("This mail has no text, or ESI no longer has it."),
        None => page = page.text("Its text is still being read."),
    }
    Ok(page)
}

/// Text in paragraphs the host takes (each under 2 KiB), split at blank
/// lines, then lines, then anywhere.
pub(crate) fn paragraphs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for block in text.split("\n\n").map(str::trim).filter(|b| !b.is_empty()) {
        let mut current = String::new();
        for line in block.lines() {
            if !current.is_empty() && current.len() + line.len() + 1 > PARAGRAPH {
                out.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
            while current.len() > PARAGRAPH {
                let mut cut = PARAGRAPH;
                while !current.is_char_boundary(cut) {
                    cut -= 1;
                }
                let rest = current.split_off(cut);
                out.push(std::mem::replace(&mut current, rest));
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_splits_under_the_hosts_limit() {
        assert_eq!(paragraphs("a\n\nb\nc"), vec!["a", "b\nc"]);
        let long = "é".repeat(3000);
        let parts = paragraphs(&long);
        assert!(parts.iter().all(|p| p.len() <= PARAGRAPH), "{parts:?}");
        assert_eq!(parts.concat(), long);
        assert!(paragraphs("  \n\n ").is_empty());
    }
}
