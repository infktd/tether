//! Turns CCP's static data export (its JSONL files, unzipped) into the
//! tab-separated tables `tether-sde` builds in: only the parts apps need,
//! in English. `scripts/update-sde.sh` runs it; nothing else does.
//!
//! sde-extract <unzipped SDE directory> <crates/sde/data>

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde_json::Value;

type Rows = Vec<Value>;

fn read(dir: &Path, file: &str) -> Result<Rows, String> {
    let path = dir.join(file);
    let f = fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    BufReader::new(f)
        .lines()
        .map(|line| {
            let line = line.map_err(|e| format!("{file}: {e}"))?;
            serde_json::from_str(&line).map_err(|e| format!("{file}: {e}"))
        })
        .collect()
}

/// A field's text, one line, no tabs.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn en(row: &Value) -> Option<String> {
    row["name"]["en"]
        .as_str()
        .map(clean)
        .filter(|n| !n.is_empty())
}

fn int(row: &Value, key: &str) -> String {
    row[key].as_i64().map(|v| v.to_string()).unwrap_or_default()
}

fn num(row: &Value, key: &str) -> String {
    row[key].as_f64().map(|v| v.to_string()).unwrap_or_default()
}

fn flag(row: &Value, key: &str) -> &'static str {
    if row[key].as_bool().unwrap_or(false) {
        "1"
    } else {
        "0"
    }
}

fn write(out: &Path, file: &str, header: &str, lines: &[String]) -> Result<(), String> {
    let path = out.join(file);
    let mut f = fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut body = String::with_capacity(lines.len() * 48);
    body.push_str(header);
    body.push('\n');
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    f.write_all(body.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn run(dir: &Path, out: &Path) -> Result<(), String> {
    let build = read(dir, "_sde.jsonl")?
        .first()
        .map(|r| {
            format!(
                "{}\t{}",
                int(r, "buildNumber"),
                r["releaseDate"].as_str().unwrap_or("")
            )
        })
        .ok_or("_sde.jsonl is empty")?;

    let compressed: HashMap<i64, i64> = read(dir, "compressibleTypes.jsonl")?
        .iter()
        .filter_map(|r| Some((r["_key"].as_i64()?, r["compressedTypeID"].as_i64()?)))
        .collect();

    let mut types: BTreeMap<i64, String> = BTreeMap::new();
    for r in read(dir, "types.jsonl")? {
        let (Some(id), Some(name)) = (r["_key"].as_i64(), en(&r)) else {
            continue;
        };
        let packaged = match num(&r, "packagedVolume") {
            p if p.is_empty() => num(&r, "volume"),
            p => p,
        };
        types.insert(
            id,
            format!(
                "{id}\t{name}\t{}\t{}\t{}\t{packaged}\t{}\t{}\t{}\t{}",
                int(&r, "groupID"),
                int(&r, "marketGroupID"),
                num(&r, "volume"),
                int(&r, "portionSize"),
                flag(&r, "published"),
                int(&r, "metaLevel"),
                compressed.get(&id).map(i64::to_string).unwrap_or_default(),
            ),
        );
    }

    let lines =
        |file: &str, f: &dyn Fn(&Value) -> Option<(i64, String)>| -> Result<Vec<String>, String> {
            let mut rows: BTreeMap<i64, String> = BTreeMap::new();
            for r in read(dir, file)? {
                if let Some((id, line)) = f(&r) {
                    rows.insert(id, line);
                }
            }
            Ok(rows.into_values().collect())
        };
    let groups = lines("groups.jsonl", &|r| {
        let id = r["_key"].as_i64()?;
        Some((
            id,
            format!(
                "{id}\t{}\t{}\t{}",
                en(r)?,
                int(r, "categoryID"),
                flag(r, "published")
            ),
        ))
    })?;
    let categories = lines("categories.jsonl", &|r| {
        let id = r["_key"].as_i64()?;
        Some((id, format!("{id}\t{}", en(r)?)))
    })?;
    let market_groups = lines("marketGroups.jsonl", &|r| {
        let id = r["_key"].as_i64()?;
        Some((id, format!("{id}\t{}\t{}", en(r)?, int(r, "parentGroupID"))))
    })?;
    let systems = lines("mapSolarSystems.jsonl", &|r| {
        let id = r["_key"].as_i64()?;
        Some((id, format!("{id}\t{}", en(r)?)))
    })?;

    let mut materials: Vec<String> = Vec::new();
    let mut rows = read(dir, "typeMaterials.jsonl")?;
    rows.sort_by_key(|r| r["_key"].as_i64().unwrap_or_default());
    for r in rows {
        let Some(id) = r["_key"].as_i64() else {
            continue;
        };
        for m in r["materials"].as_array().into_iter().flatten() {
            let mut line = String::new();
            let _ = write!(
                line,
                "{id}\t{}\t{}",
                int(m, "materialTypeID"),
                int(m, "quantity")
            );
            materials.push(line);
        }
    }

    fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    write(out, "build.tsv", "build\treleased", &[build])?;
    write(
        out,
        "types.tsv",
        "type_id\tname\tgroup_id\tmarket_group_id\tvolume\tpackaged_volume\tportion_size\tpublished\tmeta_level\tcompressed_type_id",
        &types.into_values().collect::<Vec<_>>(),
    )?;
    write(
        out,
        "groups.tsv",
        "group_id\tname\tcategory_id\tpublished",
        &groups,
    )?;
    write(out, "categories.tsv", "category_id\tname", &categories)?;
    write(
        out,
        "market_groups.tsv",
        "market_group_id\tname\tparent_id",
        &market_groups,
    )?;
    write(
        out,
        "materials.tsv",
        "type_id\tmaterial_type_id\tquantity",
        &materials,
    )?;
    write(out, "systems.tsv", "system_id\tname", &systems)?;
    Ok(())
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let (Some(dir), Some(out)) = (args.get(1), args.get(2)) else {
        let _ = writeln!(
            std::io::stderr(),
            "usage: sde-extract <unzipped SDE> <crates/sde/data>"
        );
        return std::process::ExitCode::FAILURE;
    };
    match run(Path::new(dir), Path::new(out)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "sde-extract: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}
