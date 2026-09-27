//! Moon surveys as the game copies them: after a moon probe scan, "Copy to
//! clipboard" in the Moon Analysis window gives a header line, then each
//! moon's name on a line of its own, followed by one tab-indented line per
//! ore: product, quantity (its share, 0 to 1), ore type id, solar system
//! id, planet id and moon id, tab-separated. Several moons paste at once.
//!
//! Each moon is checked on its own, so one bad moon doesn't lose the rest.

use std::fmt;

/// Moons have at most a handful of ores; more is a paste gone wrong.
pub const MAX_PRODUCTS: usize = 8;
/// Moons read from one paste.
pub const MAX_MOONS: usize = 200;
/// Shares may add up to a little over 1 from the game's rounding.
const TOTAL_SLACK: f64 = 0.001;

/// EVE's id ranges for the ids a survey carries.
const SYSTEMS: std::ops::Range<i64> = 30_000_000..33_000_000;
const CELESTIALS: std::ops::Range<i64> = 40_000_000..50_000_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Product {
    pub type_id: i64,
    /// Its share of the moon, above 0 and at most 1.
    pub amount: f64,
}

/// One moon's survey, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Survey {
    /// As pasted: shown back to the uploader, never stored (ESI names it).
    pub name: String,
    pub moon_id: i64,
    pub system_id: i64,
    pub products: Vec<Product>,
}

/// Why a moon wasn't read, for the uploader.
#[derive(Debug, Clone, PartialEq)]
pub struct Rejected {
    /// The moon's name as pasted, or where it was.
    pub moon: String,
    pub why: String,
}

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.moon, self.why)
    }
}

/// A moon as it's being read: its name line and ore lines.
struct Draft {
    name: String,
    line: usize,
    rows: Vec<(usize, Vec<String>)>,
    /// A problem found while reading its lines.
    broken: Option<String>,
}

/// Reads a pasted survey: every moon in it, each read or rejected.
pub fn parse(paste: &str) -> Vec<Result<Survey, Rejected>> {
    let mut drafts: Vec<Draft> = Vec::new();
    let mut stray: Vec<Rejected> = Vec::new();
    for (i, raw) in paste.lines().enumerate() {
        let number = i + 1;
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            continue;
        }
        let cells: Vec<&str> = line.split('\t').map(str::trim).collect();
        if is_header(&cells) {
            continue;
        }
        let fields: Vec<String> = cells
            .iter()
            .skip_while(|c| c.is_empty())
            .map(|c| (*c).to_owned())
            .collect();
        // Ore lines are indented: a tab (or spaces some tools put
        // instead) before the product. One whose indent got lost still
        // reads as an ore line by its columns (a quantity second).
        let indented = line.starts_with('\t') || line.starts_with(' ');
        let ore_line = indented || (fields.len() >= 6 && decimal(&fields[1]).is_some());
        if ore_line {
            match drafts.last_mut() {
                Some(draft) => draft.rows.push((number, fields)),
                None => stray.push(Rejected {
                    moon: format!("Line {number}"),
                    why: "an ore line before any moon's name".into(),
                }),
            }
            continue;
        }
        let name = fields.first().map(String::as_str).unwrap_or_default();
        drafts.push(Draft {
            name: shorten(name),
            line: number,
            rows: Vec::new(),
            broken: (fields.iter().filter(|f| !f.is_empty()).count() > 1)
                .then(|| format!("line {number} isn't a moon's name or an ore line")),
        });
    }
    let mut out: Vec<Result<Survey, Rejected>> = stray.into_iter().map(Err).collect();
    let mut seen: Vec<i64> = Vec::new();
    for draft in drafts {
        if out.len() >= MAX_MOONS {
            out.push(Err(Rejected {
                moon: draft.name,
                why: format!(
                    "only {MAX_MOONS} moons are read from one paste: upload the rest separately"
                ),
            }));
            break;
        }
        let result = check(draft).and_then(|survey| {
            if seen.contains(&survey.moon_id) {
                Err(Rejected {
                    moon: survey.name.clone(),
                    why: "this moon is in the paste twice: only the first was read".into(),
                })
            } else {
                seen.push(survey.moon_id);
                Ok(survey)
            }
        });
        out.push(result);
    }
    out
}

/// The header line ("Moon, Moon Product, Quantity, Ore TypeID, ..." in the
/// game's language): columns after the first, none of them numbers.
fn is_header(cells: &[&str]) -> bool {
    cells.len() >= 6
        && cells
            .iter()
            .skip(1)
            .filter(|c| !c.is_empty())
            .all(|c| decimal(c).is_none())
}

fn decimal(text: &str) -> Option<f64> {
    // Some languages write 0,25 for 0.25.
    text.replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
}

fn shorten(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(100).collect();
    if name.is_empty() {
        "A moon with no name".into()
    } else {
        name
    }
}

fn id(text: &str, range: Option<std::ops::Range<i64>>) -> Option<i64> {
    let id = text.parse::<i64>().ok().filter(|id| *id > 0)?;
    match range {
        Some(range) if !range.contains(&id) => None,
        _ => Some(id),
    }
}

fn check(draft: Draft) -> Result<Survey, Rejected> {
    let reject = |why: String| Rejected {
        moon: draft.name.clone(),
        why,
    };
    if let Some(why) = &draft.broken {
        return Err(reject(why.clone()));
    }
    if draft.rows.is_empty() {
        return Err(reject(format!(
            "no ore lines after its name (line {})",
            draft.line
        )));
    }
    if draft.rows.len() > MAX_PRODUCTS {
        return Err(reject(format!(
            "{} ore lines: a moon has at most {MAX_PRODUCTS}",
            draft.rows.len()
        )));
    }
    let mut products: Vec<Product> = Vec::new();
    let mut place: Option<(i64, i64)> = None;
    for (line, fields) in &draft.rows {
        let line = *line;
        if fields.len() < 6 {
            return Err(reject(format!(
                "line {line} has {} columns, not 6 (product, quantity, ore type, system, planet, moon)",
                fields.len()
            )));
        }
        let amount = decimal(&fields[1])
            .filter(|a| *a > 0.0 && *a <= 1.0)
            .ok_or_else(|| {
                reject(format!(
                    "line {line}: the quantity should be a share above 0 and at most 1, not {:?}",
                    fields[1]
                ))
            })?;
        let type_id = id(&fields[2], None)
            .ok_or_else(|| reject(format!("line {line}: {:?} isn't an ore type id", fields[2])))?;
        let system = id(&fields[3], Some(SYSTEMS)).ok_or_else(|| {
            reject(format!(
                "line {line}: {:?} isn't a solar system id",
                fields[3]
            ))
        })?;
        id(&fields[4], Some(CELESTIALS))
            .ok_or_else(|| reject(format!("line {line}: {:?} isn't a planet id", fields[4])))?;
        let moon = id(&fields[5], Some(CELESTIALS))
            .ok_or_else(|| reject(format!("line {line}: {:?} isn't a moon id", fields[5])))?;
        match place {
            None => place = Some((moon, system)),
            Some(p) if p != (moon, system) => {
                return Err(reject(format!(
                    "line {line} is for another moon or system than the lines before it"
                )));
            }
            Some(_) => {}
        }
        if products.iter().any(|p| p.type_id == type_id) {
            return Err(reject(format!("line {line}: ore type {type_id} twice")));
        }
        products.push(Product { type_id, amount });
    }
    let total: f64 = products.iter().map(|p| p.amount).sum();
    if total > 1.0 + TOTAL_SLACK {
        return Err(reject(format!(
            "the quantities add up to {total:.3}, more than the whole moon"
        )));
    }
    let Some((moon_id, system_id)) = place else {
        return Err(reject("no ore lines".into()));
    };
    Ok(Survey {
        name: draft.name,
        moon_id,
        system_id,
        products,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str =
        "Moon\tMoon Product\tQuantity\tOre TypeID\tSolarSystemID\tPlanetID\tMoonID";

    fn ok(result: &Result<Survey, Rejected>) -> &Survey {
        result.as_ref().unwrap()
    }

    fn err(result: &Result<Survey, Rejected>) -> &Rejected {
        result.as_ref().unwrap_err()
    }

    #[test]
    fn reads_several_moons_as_the_game_copies_them() {
        let paste = format!(
            "{HEADER}\r\n\
             Jita IV - Moon 4\r\n\
             \tZeolites\t0.270251214504\t45490\t30000142\t40009077\t40009081\r\n\
             \tSylvite\t0.288750767708\t45491\t30000142\t40009077\t40009081\r\n\
             \tChromite\t0.194089412689\t45501\t30000142\t40009077\t40009081\r\n\
             \tCarnotite\t0.246908605099\t45502\t30000142\t40009077\t40009081\r\n\
             Jita IV - Moon 5\r\n\
             \tBitumens\t0.5\t45492\t30000142\t40009077\t40009082\r\n\
             \tXenotime\t0.25\t45510\t30000142\t40009077\t40009082\r\n"
        );
        let moons = parse(&paste);
        assert_eq!(moons.len(), 2, "{moons:?}");
        let first = ok(&moons[0]);
        assert_eq!(first.name, "Jita IV - Moon 4");
        assert_eq!((first.moon_id, first.system_id), (40009081, 30000142));
        assert_eq!(first.products.len(), 4);
        assert_eq!(
            first.products[0],
            Product {
                type_id: 45490,
                amount: 0.270251214504
            }
        );
        let second = ok(&moons[1]);
        assert_eq!(second.moon_id, 40009082);
        assert_eq!(
            second
                .products
                .iter()
                .map(|p| p.type_id)
                .collect::<Vec<_>>(),
            vec![45492, 45510]
        );
    }

    #[test]
    fn a_bad_moon_is_rejected_on_its_own() {
        let paste = format!(
            "{HEADER}\n\
             Good Moon\n\
             \tZeolites\t0.4\t45490\t30000142\t40009077\t40009081\n\
             Too Much Moon\n\
             \tZeolites\t0.8\t45490\t30000142\t40009077\t40009083\n\
             \tSylvite\t0.8\t45491\t30000142\t40009077\t40009083\n\
             Short Moon\n\
             \tZeolites\t0.4\t45490\n\
             Mixed Moon\n\
             \tZeolites\t0.4\t45490\t30000142\t40009077\t40009084\n\
             \tSylvite\t0.4\t45491\t30000142\t40009077\t40009085\n\
             Bad Numbers\n\
             \tZeolites\tlots\t45490\t30000142\t40009077\t40009086\n\
             Empty Moon\n\
             Bad System\n\
             \tZeolites\t0.4\t45490\t99\t40009077\t40009087\n"
        );
        let moons = parse(&paste);
        assert_eq!(moons.len(), 7, "{moons:?}");
        assert_eq!(ok(&moons[0]).moon_id, 40009081);
        let why: Vec<(&str, &str)> = moons[1..]
            .iter()
            .map(|m| (err(m).moon.as_str(), err(m).why.as_str()))
            .collect();
        assert_eq!(why[0].0, "Too Much Moon");
        assert!(why[0].1.contains("add up to 1.600"), "{why:?}");
        assert_eq!(why[1].0, "Short Moon");
        assert!(why[1].1.contains("3 columns, not 6"), "{why:?}");
        assert!(why[2].1.contains("another moon"), "{why:?}");
        assert!(why[3].1.contains("quantity"), "{why:?}");
        assert!(why[4].1.contains("no ore lines"), "{why:?}");
        assert!(why[5].1.contains("solar system"), "{why:?}");
    }

    #[test]
    fn stray_lines_duplicates_and_commas() {
        let paste = "\tZeolites\t0.4\t45490\t30000142\t40009077\t40009081\n\
                     Mond 1\n\
                     \tZeolites\t0,4\t45490\t30000142\t40009077\t40009081\n\
                     \tSylvite\t0,6\t45491\t30000142\t40009077\t40009081\n\
                     Mond 1 again\n\
                     \tZeolites\t0.4\t45490\t30000142\t40009077\t40009081\n";
        let moons = parse(paste);
        assert_eq!(moons.len(), 3, "{moons:?}");
        assert_eq!(err(&moons[0]).moon, "Line 1");
        let read = ok(&moons[1]);
        assert_eq!(read.products[0].amount, 0.4);
        assert_eq!(read.products[1].amount, 0.6);
        assert!(err(&moons[2]).why.contains("twice"));
    }

    #[test]
    fn a_localized_header_is_skipped_and_a_lost_indent_still_reads() {
        let paste = "Mond\tMondprodukt\tMenge\tErz-TypID\tSonnensystemID\tPlanetID\tMondID\n\
                     Jita IV - Moon 4\n\
                     Zeolites\t0.3\t45490\t30000142\t40009077\t40009081\n";
        let moons = parse(paste);
        // Without its indent the ore line still reads by its columns.
        assert_eq!(moons.len(), 1, "{moons:?}");
        assert_eq!(ok(&moons[0]).name, "Jita IV - Moon 4");
        assert_eq!(ok(&moons[0]).moon_id, 40009081);
        assert!(parse(HEADER).is_empty());
        assert!(parse("").is_empty());
        // Something that's neither a name nor an ore line.
        let odd = parse(
            "Jita IV - Moon 4\tsomething\n\tZeolites\t0.3\t45490\t30000142\t40009077\t40009081\n",
        );
        assert!(err(&odd[0]).why.contains("isn't a moon's name"), "{odd:?}");
    }

    #[test]
    fn repeated_ores_and_too_many_lines() {
        let row = "\tZeolites\t0.1\t45490\t30000142\t40009077\t40009081\n";
        let twice = format!("Moon A\n{row}{row}");
        assert!(err(&parse(&twice)[0]).why.contains("twice"));
        let mut many = "Moon B\n".to_owned();
        for t in 0..9 {
            many.push_str(&format!(
                "\tOre\t0.1\t{}\t30000142\t40009077\t40009081\n",
                45490 + t
            ));
        }
        assert!(err(&parse(&many)[0]).why.contains("at most 8"));
    }
}
