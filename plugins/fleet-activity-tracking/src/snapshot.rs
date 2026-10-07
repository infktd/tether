//! aa-afat's fleet snapshot: the fleet composition an FC copies from EVE's
//! fleet window, one member a line, tab separated: name, system, ship
//! type, ship group, position, skills ("5 - 5 - 5") and wing / squad.

/// EVE's largest fleet.
pub(crate) const MAX_MEMBERS: usize = 256;

/// A fleet member as the fleet window copies them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) name: String,
    pub(crate) system: String,
    pub(crate) ship: String,
}

/// The skills column: fleet, wing and squad command, each 0 to 5.
fn skills(column: &str) -> bool {
    let parts: Vec<&str> = column.split(" - ").collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| p.len() == 1 && matches!(p.as_bytes()[0], b'0'..=b'5'))
}

/// The members in a pasted fleet composition, each once, or why it isn't
/// one. Every line must be a member (aa-afat checks the first and fails on
/// any other that isn't; this says which).
pub(crate) fn parse(text: &str) -> Result<Vec<Member>, String> {
    let mut members: Vec<Member> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_end_matches(['\r', '\t']);
        if line.trim().is_empty() {
            continue;
        }
        // Tabs in a row count as one, as aa-afat splits them.
        let columns: Vec<&str> = line.split('\t').filter(|c| !c.is_empty()).collect();
        let name = columns.first().map_or("", |c| c.trim());
        let ok = columns.len() >= 6
            && (3..=37).contains(&name.chars().count())
            && skills(columns[5].trim());
        if !ok {
            return Err(format!(
                "Line {} isn't a fleet member as EVE's fleet window copies them. Copy the fleet \
                 composition from the fleet window and paste it as it is.",
                n + 1
            ));
        }
        if members.iter().any(|m| m.name.eq_ignore_ascii_case(name)) {
            continue;
        }
        if members.len() == MAX_MEMBERS {
            return Err(format!(
                "A fleet has at most {MAX_MEMBERS} members; this has more lines than that."
            ));
        }
        members.push(Member {
            name: name.to_owned(),
            system: columns[1].trim().to_owned(),
            ship: columns[2].trim().to_owned(),
        });
    }
    if members.is_empty() {
        return Err("Paste the fleet composition from EVE's fleet window.".to_owned());
    }
    Ok(members)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASTE: &str = "Line Member\tJita\tRokh\tBattleship\tSquad Member\t0 - 0 - 5\tWing 1 / Squad 1\n\
                         Line Alt\tJita\tScimitar\tLogistics\tSquad Commander\t\t1 - 2 - 3\tWing 1 / Squad 1\r\n\
                         \n\
                         gigX\tPerimeter\tRokh\tBattleship\tFleet Commander\t5 - 5 - 5\t\n";

    #[test]
    fn reads_the_fleet_window() {
        let members = parse(PASTE).unwrap();
        assert_eq!(
            members,
            vec![
                Member {
                    name: "Line Member".into(),
                    system: "Jita".into(),
                    ship: "Rokh".into()
                },
                Member {
                    name: "Line Alt".into(),
                    system: "Jita".into(),
                    ship: "Scimitar".into()
                },
                Member {
                    name: "gigX".into(),
                    system: "Perimeter".into(),
                    ship: "Rokh".into()
                },
            ]
        );
    }

    #[test]
    fn each_member_once() {
        let twice = format!("{PASTE}GIGX\tJita\tRokh\tBattleship\tSquad Member\t0 - 0 - 0\tWing 1");
        assert_eq!(parse(&twice).unwrap().len(), 3);
    }

    #[test]
    fn refuses_what_isnt_a_fleet_composition() {
        assert!(parse("").is_err());
        assert!(parse("  \n\n").is_err());
        let err = parse(&format!("{PASTE}Some text pasted by mistake")).unwrap_err();
        assert!(err.starts_with("Line 5 "), "{err}");
        // Skills out of range, or a name too short.
        assert!(parse("Line Member\tJita\tRokh\tBattleship\tSquad Member\t0 - 0 - 6").is_err());
        assert!(parse("Li\tJita\tRokh\tBattleship\tSquad Member\t0 - 0 - 5").is_err());
    }

    #[test]
    fn a_fleet_has_at_most_256_members() {
        let line =
            |n: usize| format!("Pilot {n}\tJita\tRokh\tBattleship\tSquad Member\t0 - 0 - 0\n");
        let full: String = (0..MAX_MEMBERS).map(line).collect();
        assert_eq!(parse(&full).unwrap().len(), MAX_MEMBERS);
        assert!(parse(&format!("{full}{}", line(MAX_MEMBERS))).is_err());
    }
}
