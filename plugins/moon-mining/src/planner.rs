//! The extraction planner, for Station Managers: a pop cadence (one moon
//! every N hours from an EVE time of day) turned into the duration to set
//! at each drill so its chunk pops on a free slot, counting the automatic
//! fracture three hours after the chunk arrives. ESI can't start
//! extractions, so this only advises.

use chrono::{DateTime, Duration, NaiveTime, Utc};

/// A chunk fractures by itself this long after it arrives.
pub const AUTO_FRACTURE: Duration = Duration::hours(3);
/// The shortest and longest extraction the game allows.
pub const MIN_EXTRACTION: Duration = Duration::days(6);
pub const MAX_EXTRACTION: Duration = Duration::days(56);
/// A pop this close to its slot counts as on it.
pub const ON_SLOT: Duration = Duration::minutes(30);
/// How far ahead gaps are looked for.
pub const GAP_HORIZON: Duration = Duration::days(14);

/// When pops are wanted: every `every` hours, lined up on `at` (EVE time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    pub every: Duration,
    pub at: NaiveTime,
}

impl Cadence {
    /// The slot nearest `t`.
    pub fn nearest(&self, t: DateTime<Utc>) -> DateTime<Utc> {
        let anchor = t.date_naive().and_time(self.at).and_utc();
        let every = self.every.num_seconds().max(3600);
        let offset = (t - anchor).num_seconds();
        let k = (offset as f64 / every as f64).round() as i64;
        anchor + Duration::seconds(k * every)
    }

    /// The first slot at or after `t`.
    pub fn first_from(&self, t: DateTime<Utc>) -> DateTime<Utc> {
        let slot = self.nearest(t);
        if slot < t { slot + self.every } else { slot }
    }
}

/// A drill (an Athanor or Tatara with a moon), and its current chunk's pop
/// (automatic fracture), if it has one coming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drill {
    pub structure_id: i64,
    pub name: String,
    pub pop: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Advice {
    /// Pops on its slot.
    OnSlot {
        pop: DateTime<Utc>,
        slot: DateTime<Utc>,
    },
    /// Pops, but off the cadence by this much (positive: late).
    OffSlot {
        pop: DateTime<Utc>,
        slot: DateTime<Utc>,
        off: Duration,
    },
    /// Pops in the same slot as another drill.
    Overlap {
        pop: DateTime<Utc>,
        slot: DateTime<Utc>,
        with: String,
    },
    /// Idle: start an extraction now of `duration` (the chunk arrives at
    /// `arrival`) to pop on `slot`.
    Start {
        slot: DateTime<Utc>,
        arrival: DateTime<Utc>,
        duration: Duration,
    },
    /// Idle, and no free slot within the longest extraction.
    NoSlot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Per drill, in the drills' order.
    pub advice: Vec<(Drill, Advice)>,
    /// Slots in the next two weeks nothing pops on.
    pub gaps: Vec<DateTime<Utc>>,
}

/// Plans every drill against the cadence.
pub fn plan(drills: &[Drill], cadence: Cadence, now: DateTime<Utc>) -> Plan {
    let mut taken: Vec<(DateTime<Utc>, String)> = Vec::new();
    let mut advice: Vec<Option<Advice>> = vec![None; drills.len()];
    // Drills already extracting keep their pops.
    for (i, drill) in drills.iter().enumerate() {
        let Some(pop) = drill.pop.filter(|p| *p > now) else {
            continue;
        };
        let slot = cadence.nearest(pop);
        if let Some((_, other)) = taken.iter().find(|(s, _)| *s == slot) {
            advice[i] = Some(Advice::Overlap {
                pop,
                slot,
                with: other.clone(),
            });
            continue;
        }
        taken.push((slot, drill.name.clone()));
        let off = pop - slot;
        advice[i] = Some(if off.abs() <= ON_SLOT {
            Advice::OnSlot { pop, slot }
        } else {
            Advice::OffSlot { pop, slot, off }
        });
    }
    // Idle drills take the earliest free slots they can reach.
    let earliest = now + MIN_EXTRACTION + AUTO_FRACTURE;
    let latest = now + MAX_EXTRACTION + AUTO_FRACTURE;
    for (i, drill) in drills.iter().enumerate() {
        if advice[i].is_some() {
            continue;
        }
        let mut slot = cadence.first_from(earliest);
        while slot <= latest && taken.iter().any(|(s, _)| *s == slot) {
            slot += cadence.every;
        }
        advice[i] = Some(if slot <= latest {
            taken.push((slot, drill.name.clone()));
            let arrival = slot - AUTO_FRACTURE;
            Advice::Start {
                slot,
                arrival,
                duration: arrival - now,
            }
        } else {
            Advice::NoSlot
        });
    }
    let mut gaps = Vec::new();
    let mut slot = cadence.first_from(now);
    while slot <= now + GAP_HORIZON {
        if !taken.iter().any(|(s, _)| *s == slot) {
            gaps.push(slot);
        }
        slot += cadence.every;
    }
    Plan {
        advice: drills
            .iter()
            .cloned()
            .zip(advice.into_iter().map(|a| a.unwrap_or(Advice::NoSlot)))
            .collect(),
        gaps,
    }
}

/// `12d 4h 30m`, as the game shows extraction times.
pub fn duration_text(d: Duration) -> String {
    let minutes = d.num_minutes().max(0);
    let (days, hours, mins) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 || days > 0 {
        parts.push(format!("{hours}h"));
    }
    parts.push(format!("{mins}m"));
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn daily_at_19() -> Cadence {
        Cadence {
            every: Duration::hours(24),
            at: NaiveTime::from_hms_opt(19, 0, 0).unwrap(),
        }
    }

    fn drill(id: i64, pop: Option<&str>) -> Drill {
        Drill {
            structure_id: id,
            name: format!("Athanor {id}"),
            pop: pop.map(at),
        }
    }

    #[test]
    fn slots_line_up_on_the_time_of_day() {
        let c = daily_at_19();
        assert_eq!(
            c.nearest(at("2026-09-25T20:10:00Z")),
            at("2026-09-25T19:00:00Z")
        );
        assert_eq!(
            c.nearest(at("2026-09-26T08:00:00Z")),
            at("2026-09-26T19:00:00Z")
        );
        assert_eq!(
            c.first_from(at("2026-09-25T19:00:01Z")),
            at("2026-09-26T19:00:00Z")
        );
        let every_8h = Cadence {
            every: Duration::hours(8),
            at: NaiveTime::from_hms_opt(3, 0, 0).unwrap(),
        };
        assert_eq!(
            every_8h.first_from(at("2026-09-25T12:00:00Z")),
            at("2026-09-25T19:00:00Z")
        );
    }

    #[test]
    fn idle_drills_get_the_duration_for_the_next_free_slots() {
        let now = at("2026-09-25T12:00:00Z");
        let plan = plan(
            &[
                drill(1, None),
                drill(2, None),
                drill(3, Some("2026-10-02T19:10:00Z")),
            ],
            daily_at_19(),
            now,
        );
        // Drill 3 already pops on the 2 Oct slot (10 minutes late: on it).
        assert_eq!(
            plan.advice[2].1,
            Advice::OnSlot {
                pop: at("2026-10-02T19:10:00Z"),
                slot: at("2026-10-02T19:00:00Z")
            }
        );
        // The earliest reachable pop is now + 6d + 3h = 1 Oct 15:00, so the
        // 1 Oct 19:00 slot; the chunk must arrive three hours before.
        assert_eq!(
            plan.advice[0].1,
            Advice::Start {
                slot: at("2026-10-01T19:00:00Z"),
                arrival: at("2026-10-01T16:00:00Z"),
                duration: Duration::hours(6 * 24 + 4),
            }
        );
        // 2 Oct is taken, so the next idle drill goes to 3 Oct.
        match &plan.advice[1].1 {
            Advice::Start { slot, .. } => assert_eq!(*slot, at("2026-10-03T19:00:00Z")),
            other => panic!("{other:?}"),
        }
        // Nothing pops in the first six days: those are gaps.
        assert_eq!(plan.gaps.first(), Some(&at("2026-09-25T19:00:00Z")));
        assert!(!plan.gaps.contains(&at("2026-10-01T19:00:00Z")));
    }

    #[test]
    fn overlaps_and_off_slot_pops_are_flagged() {
        let now = at("2026-09-25T12:00:00Z");
        let plan = plan(
            &[
                drill(1, Some("2026-09-28T19:00:00Z")),
                drill(2, Some("2026-09-28T20:00:00Z")),
                drill(3, Some("2026-09-30T23:00:00Z")),
            ],
            daily_at_19(),
            now,
        );
        assert!(matches!(&plan.advice[1].1, Advice::Overlap { with, .. } if with == "Athanor 1"));
        assert!(
            matches!(plan.advice[2].1, Advice::OffSlot { off, .. } if off == Duration::hours(4))
        );
    }

    #[test]
    fn durations_read_like_the_game() {
        assert_eq!(
            duration_text(Duration::minutes(6 * 1440 + 4 * 60 + 5)),
            "6d 4h 5m"
        );
        assert_eq!(duration_text(Duration::minutes(59)), "59m");
    }
}
