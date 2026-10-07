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
}
