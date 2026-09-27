//! App pages' instruments (DESIGN.md, Components): skill levels as EVE's
//! five squares, compositions as rings, defenses as shield, armor and hull
//! rings, and timelines of lanes. The page checker (`tether_plugins::page`)
//! has already bounded every number and time; this only lays them out.
//! Colours come from fixed tables here, never from a plugin.

use chrono::{DateTime, Duration, Utc};
use tether_plugins::host::{Composition, Defenses, Levels, Timeline, Tone};

use crate::plugins::page_href;

/// The rarity scale, darker to brighter by value (DESIGN.md, Tokens): a
/// moon's R4 to R64, as `grade-0` to `grade-4` in the stylesheet (the CSP
/// allows no inline styles, so colours and positions are classes).
const GRADES: [&str; 5] = ["grade-0", "grade-1", "grade-2", "grade-3", "grade-4"];

pub struct LevelsView {
    /// `trained`, `training` or `` for each of the five squares.
    pub cells: Vec<&'static str>,
    pub label: String,
}

pub fn levels(levels: &Levels) -> LevelsView {
    let cells = (1..=5u8)
        .map(|level| {
            if level <= levels.trained {
                "trained"
            } else if levels.training == Some(level) {
                "training"
            } else {
                ""
            }
        })
        .collect();
    let label = match levels.training {
        Some(training) => format!("Level {} of 5, training {training}", levels.trained),
        None => format!("Level {} of 5", levels.trained),
    };
    LevelsView { cells, label }
}

/// One arc of a ring: an SVG circle's dash pattern and its colour class.
pub struct Arc {
    pub color: &'static str,
    pub dash: String,
    pub offset: String,
}

pub struct CompositionView {
    /// Large, with `center` in the middle; else a small ring for a table.
    pub large: bool,
    pub size: u32,
    pub mid: f64,
    pub radius: f64,
    pub stroke: f64,
    /// The disc inside the ring.
    pub disc: f64,
    pub arcs: Vec<Arc>,
    pub center: Option<String>,
    /// Each part with its share, for the legend and the label.
    pub parts: Vec<(String, u32, &'static str)>,
    pub label: String,
}

pub fn composition(c: &Composition) -> CompositionView {
    let large = c.center.is_some();
    let (size, stroke) = if large { (180u32, 12.0) } else { (36u32, 4.0) };
    let mid = f64::from(size) / 2.0;
    let radius = mid - stroke / 2.0 - 1.0;
    let circumference = 2.0 * std::f64::consts::PI * radius;
    let total: f64 = c.parts.iter().map(|p| p.amount).sum();
    // A hairline between parts, unless there is only one.
    let gap = if c.parts.len() > 1 {
        if large { 2.0 } else { 1.5 }
    } else {
        0.0
    };
    let mut done = 0.0;
    let mut arcs = Vec::new();
    let mut parts = Vec::new();
    for part in &c.parts {
        let fraction = if total > 0.0 {
            part.amount / total
        } else {
            0.0
        };
        let color = GRADES[usize::from(part.grade.min(4))];
        let length = (circumference * fraction - gap).max(0.0);
        arcs.push(Arc {
            color,
            dash: format!("{length:.2} {:.2}", circumference - length),
            offset: format!("{:.2}", -done),
        });
        done += circumference * fraction;
        parts.push((part.label.clone(), (fraction * 100.0).round() as u32, color));
    }
    let label = parts
        .iter()
        .map(|(name, pct, _)| format!("{name} {pct}%"))
        .collect::<Vec<_>>()
        .join(", ");
    CompositionView {
        large,
        size,
        mid,
        radius,
        stroke,
        disc: radius - stroke / 2.0 - if large { 10.0 } else { 3.0 },
        arcs,
        center: c.center.clone(),
        parts,
        label,
    }
}

pub struct DefensesView {
    /// Shield, armor and hull, outside in: the track and the part left.
    pub rings: Vec<(f64, Arc, Arc)>,
    pub alarm: bool,
    pub label: String,
}

/// Three-quarter rings, as EVE's HUD: each starts bottom-left and runs
/// clockwise.
pub fn defenses(d: &Defenses) -> DefensesView {
    let ring = |radius: f64, value: f64, healthy: &'static str| {
        let circumference = 2.0 * std::f64::consts::PI * radius;
        let whole = circumference * 0.75;
        let left = whole * value;
        let track = Arc {
            color: "arc-track",
            dash: format!("{whole:.2} {:.2}", circumference - whole),
            offset: "0".to_owned(),
        };
        let part = Arc {
            // Damage shows red.
            color: if value < 1.0 { "arc-danger" } else { healthy },
            dash: format!("{left:.2} {:.2}", circumference - left),
            offset: "0".to_owned(),
        };
        (radius, track, part)
    };
    DefensesView {
        rings: vec![
            ring(19.0, d.shield, "arc-info"),
            ring(14.5, d.armor, "arc-fog"),
            ring(10.0, d.hull, "arc-bone"),
        ],
        alarm: d.alarm,
        label: format!(
            "Shield {}%, armor {}%, hull {}%{}",
            (d.shield * 100.0).round(),
            (d.armor * 100.0).round(),
            (d.hull * 100.0).round(),
            if d.alarm { ", alarm" } else { "" }
        ),
    }
}

pub struct TimelineView {
    pub title: Option<String>,
    /// Day ticks: where (a position class) and the label.
    pub ticks: Vec<(String, String)>,
    /// Shaded windows: position and width classes.
    pub windows: Vec<(String, String)>,
    /// Where now is, when the span holds it.
    pub now: Option<String>,
    pub lanes: Vec<LaneView>,
}

pub struct LaneView {
    pub label: String,
    pub caption: Option<String>,
    pub items: Vec<ItemView>,
}

pub struct ItemView {
    pub label: String,
    /// Left edge, and for a bar its width (position and width classes).
    pub left: String,
    pub width: Option<String>,
    /// Two rows per lane, so neighbours don't sit on each other.
    pub row: usize,
    /// `signal`, `danger`, `info` or ``.
    pub tone: &'static str,
    pub planned: bool,
    pub href: Option<String>,
    /// The EVE time, and the time left (or `done`).
    pub when: String,
    pub left_text: String,
}

fn utc(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// A position or width along a timeline, as the stylesheet's class for it:
/// half-percent steps (`tl-l0` to `tl-l200`, `tl-w1` to `tl-w200`).
fn left(x: f64) -> String {
    format!("tl-l{}", (x.clamp(0.0, 1.0) * 200.0).round() as u32)
}

fn width(x: f64) -> String {
    format!(
        "tl-w{}",
        ((x.clamp(0.0, 1.0) * 200.0).round() as u32).max(1)
    )
}

pub fn timeline(plugin: &str, t: &Timeline, now: DateTime<Utc>) -> TimelineView {
    // Checked already: both parse and `to` is after `from`.
    let from = utc(&t.from).unwrap_or(now);
    let to = utc(&t.to).unwrap_or(from + Duration::days(1));
    let span = (to - from).num_seconds().max(1) as f64;
    let at = |instant: DateTime<Utc>| (instant - from).num_seconds() as f64 / span;

    // A tick at each EVE midnight in the span (at most 60).
    let mut ticks = Vec::new();
    let mut day = from.date_naive().and_hms_opt(0, 0, 0).map(|d| d.and_utc());
    while let Some(d) = day {
        if d >= to {
            break;
        }
        if d > from {
            ticks.push((left(at(d)), d.format("%a %d").to_string().to_uppercase()));
        }
        day = d.checked_add_signed(Duration::days(1));
    }
    let windows = t
        .windows
        .iter()
        .filter_map(|w| {
            let (a, b) = (utc(&w.from)?, utc(&w.to)?);
            let (a, b) = (at(a).clamp(0.0, 1.0), at(b).clamp(0.0, 1.0));
            (b > a).then(|| (left(a), width(b - a)))
        })
        .collect();
    let lanes = t
        .lanes
        .iter()
        .map(|lane| LaneView {
            label: lane.label.clone(),
            caption: lane.caption.clone(),
            items: lane
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    let start = utc(&item.at)?;
                    let end = item.until.as_deref().and_then(utc);
                    // Outside the span entirely: not drawn.
                    if end.unwrap_or(start) < from || start > to {
                        return None;
                    }
                    let x = at(start).clamp(0.0, 1.0);
                    let w = end.map(|e| at(e).clamp(0.0, 1.0) - x);
                    let seconds = (start - now).num_seconds();
                    Some(ItemView {
                        label: item.label.clone(),
                        left: left(x),
                        width: w.map(width),
                        row: i % 2,
                        tone: match item.tone {
                            Tone::Warning | Tone::Accent => "signal",
                            Tone::Danger => "danger",
                            Tone::Success => "info",
                            Tone::Neutral => "",
                        },
                        planned: item.planned,
                        href: item.link.as_deref().map(|p| page_href(plugin, p)),
                        when: format!("{} EVE", start.format("%a %H:%M")),
                        left_text: super::plugin_pages::countdown_text(seconds),
                    })
                })
                .collect(),
        })
        .collect();
    TimelineView {
        title: t.title.clone(),
        ticks,
        windows,
        now: (from <= now && now <= to).then(|| left(at(now))),
        lanes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tether_plugins::host::{Lane, LaneItem, Share, Window};

    #[test]
    fn levels_mark_trained_and_training() {
        let v = levels(&Levels {
            trained: 3,
            training: Some(4),
        });
        assert_eq!(
            v.cells,
            vec!["trained", "trained", "trained", "training", ""]
        );
        assert_eq!(v.label, "Level 3 of 5, training 4");
    }

    #[test]
    fn a_composition_splits_the_ring_by_amount() {
        let part = |amount, grade| Share {
            label: "ore".to_owned(),
            amount,
            grade,
        };
        let v = composition(&Composition {
            parts: vec![part(1.0, 4), part(3.0, 0)],
            center: None,
        });
        assert!(!v.large);
        assert_eq!(v.parts[0].1, 25);
        assert_eq!(v.parts[1].1, 75);
        assert_eq!(v.arcs[0].color, "grade-4");
        assert!(v.arcs[1].offset.starts_with('-'));
        let big = composition(&Composition {
            parts: vec![part(1.0, 2)],
            center: Some("1.84B".to_owned()),
        });
        assert!(big.large);
        assert_eq!(big.size, 180);
    }

    #[test]
    fn damage_shows_red() {
        let v = defenses(&Defenses {
            shield: 0.0,
            armor: 0.62,
            hull: 1.0,
            alarm: true,
        });
        assert_eq!(v.rings[0].2.color, "arc-danger");
        assert_eq!(v.rings[1].2.color, "arc-danger");
        assert_eq!(v.rings[2].2.color, "arc-bone");
        assert!(v.label.ends_with("alarm"));
    }

    #[test]
    fn a_timeline_places_items_ticks_and_now() {
        let now = utc("2026-09-27T12:00:00Z").unwrap();
        let t = Timeline {
            title: None,
            from: "2026-09-27T00:00:00Z".to_owned(),
            to: "2026-09-29T00:00:00Z".to_owned(),
            lanes: vec![Lane {
                label: "Fleets".to_owned(),
                caption: None,
                items: vec![
                    LaneItem {
                        label: "Stratop".to_owned(),
                        at: "2026-09-28T00:00:00Z".to_owned(),
                        until: Some("2026-09-28T12:00:00Z".to_owned()),
                        tone: Tone::Warning,
                        planned: false,
                        link: Some("op/1".to_owned()),
                    },
                    LaneItem {
                        label: "Gone".to_owned(),
                        at: "2026-09-20T00:00:00Z".to_owned(),
                        until: None,
                        tone: Tone::Neutral,
                        planned: true,
                        link: None,
                    },
                ],
            }],
            windows: vec![Window {
                from: "2026-09-27T18:00:00Z".to_owned(),
                to: "2026-09-27T21:00:00Z".to_owned(),
            }],
        };
        let v = timeline("acme.ops", &t, now);
        assert_eq!(v.now.as_deref(), Some("tl-l50"));
        assert_eq!(v.ticks, vec![("tl-l100".to_owned(), "MON 28".to_owned())]);
        assert_eq!(v.windows, vec![("tl-l75".to_owned(), "tl-w13".to_owned())]);
        // The one before the span isn't drawn.
        assert_eq!(v.lanes[0].items.len(), 1);
        let item = &v.lanes[0].items[0];
        assert_eq!(item.left, "tl-l100");
        assert_eq!(item.width.as_deref(), Some("tl-w50"));
        assert_eq!(item.tone, "signal");
        assert_eq!(item.href.as_deref(), Some("/plugins/acme.ops/op/1"));
    }
}
