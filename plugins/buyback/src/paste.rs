//! The calculator's paste (aa-buybackprogram `views/calculate.py`): lines
//! copied from the game's inventory, tab-separated, in its list or
//! details view. Each line is a row; names are taken as written, without
//! `*` (the game's mark for an item in a container's own list).

/// One pasted line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub name: String,
    pub quantity: i64,
    /// Assembled (a fitted ship, an anchored container): no quantity.
    pub unpacked: bool,
}

/// A paste with no tab anywhere isn't from the game's inventory.
pub fn is_inventory_paste(text: &str) -> bool {
    text.contains('\t')
}

/// The digits of a quantity in any locale ("1,000", "1 000", "1.000").
fn quantity(field: &str) -> Option<i64> {
    let digits: String = field.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The lines of a paste, in order; blank lines skipped.
pub fn lines(text: &str) -> Vec<Line> {
    text.split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let parts: Vec<&str> = l.split('\t').collect();
            let name = parts[0].replace('*', "").trim().to_owned();
            match parts.get(1).map(|q| q.trim()) {
                // A name alone, or an empty quantity: assembled.
                None | Some("") => Line {
                    name,
                    quantity: 1,
                    unpacked: true,
                },
                Some(q) => match quantity(q) {
                    Some(n) => Line {
                        name,
                        quantity: n,
                        unpacked: false,
                    },
                    // No digits where the quantity goes (B15): counted as
                    // one, assembled, rather than failing the paste.
                    None => Line {
                        name,
                        quantity: 1,
                        unpacked: true,
                    },
                },
            }
        })
        .collect()
}

/// The lines with one row a name (packed and assembled apart), their
/// quantities added, in the order each first appears: a paste of a
/// big hangar lists a type many times, and its rows are kept and shown.
pub fn merged(lines: Vec<Line>) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let mut at: std::collections::HashMap<(String, bool), usize> = std::collections::HashMap::new();
    for line in lines {
        match at.get(&(line.name.clone(), line.unpacked)) {
            Some(&i) => out[i].quantity = out[i].quantity.saturating_add(line.quantity),
            None => {
                at.insert((line.name.clone(), line.unpacked), out.len());
                out.push(line);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_and_details_views_read() {
        let paste = "Tritanium\t1,000\r\nVeldspar*\t2 500\tVeldspar\tAsteroid\t\t\t250 m3\t25,000 ISK\nRifter\t\tFrigate\tShip\n\nPyerite\tlots\n";
        assert!(is_inventory_paste(paste));
        let lines = lines(paste);
        assert_eq!(lines.len(), 4);
        assert_eq!(
            lines[0],
            Line {
                name: "Tritanium".into(),
                quantity: 1000,
                unpacked: false
            }
        );
        assert_eq!(
            lines[1],
            Line {
                name: "Veldspar".into(),
                quantity: 2500,
                unpacked: false
            }
        );
        assert_eq!(
            lines[2],
            Line {
                name: "Rifter".into(),
                quantity: 1,
                unpacked: true
            }
        );
        assert!(lines[3].unpacked);
        assert!(!is_inventory_paste("Tritanium 1000"));
    }

    #[test]
    fn repeated_names_are_one_row() {
        let merged = merged(lines(
            "Tritanium\t10\nRifter\t\nTritanium\t5\nRifter\t\nRifter\t1\n",
        ));
        assert_eq!(merged.len(), 3);
        assert_eq!(
            (merged[0].name.as_str(), merged[0].quantity),
            ("Tritanium", 15)
        );
        assert_eq!((merged[1].quantity, merged[1].unpacked), (2, true));
        assert_eq!((merged[2].quantity, merged[2].unpacked), (1, false));
    }
}
