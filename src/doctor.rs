//! Explicit, backed-up migration of managed library registrations and references.
use crate::{
    config::Config,
    destination::{self, Destination},
    importer,
    sexpr::{Node, parse},
    storage::{Lock, Plan, Record},
};
use anyhow::{Context, Result, bail};
use std::{collections::BTreeMap, fs, path::Path};

type Renames = BTreeMap<String, String>;

fn table(dest: &Destination, kind: &str, vars: &Renames, plan: &mut Plan) -> Result<Renames> {
    let (file, target) = if kind == "sym_lib_table" {
        ("sym-lib-table", &dest.symbol_path)
    } else {
        ("fp-lib-table", &dest.pretty_dir)
    };
    let path = dest.table_dir.join(file);
    if !path.exists() {
        return Ok(Renames::new());
    }
    let original = fs::read_to_string(&path)?;
    let mut node = parse(&original)?;
    if node.head() != Some(kind) {
        bail!("Invalid table {}", path.display());
    }
    // Keep exact old names so a later project migration also handles legacy
    // collision suffixes without guessing which nickname used to be ours.
    let history_path = crate::config::data_dir()
        .join("nickname-migrations")
        .join(format!(
            "{}.json",
            crate::storage::hash(path.to_string_lossy().as_bytes())
        ));
    let mut history: Renames = if history_path.exists() {
        serde_json::from_slice(&fs::read(&history_path)?)?
    } else {
        Renames::new()
    };
    let original_history = history.clone();
    let mut changes = Renames::new();
    let mut occupied = BTreeMap::new();
    for entry in node.children().iter().filter(|n| n.head() == Some("lib")) {
        let name = entry
            .field("name")
            .and_then(Node::value)
            .context("Library entry missing name")?;
        if occupied.insert(name.to_owned(), ()).is_some() {
            bail!("Duplicate nickname {name} in {}", path.display());
        }
    }
    for entry in node
        .children_mut()
        .iter_mut()
        .filter(|n| n.head() == Some("lib"))
    {
        let name = entry
            .field("name")
            .and_then(Node::value)
            .context("Library entry missing name")?
            .to_owned();
        // Only change entries that resolve to our managed file/directory.
        let uri = entry
            .field("uri")
            .and_then(Node::value)
            .context("Library entry missing URI")?;
        let Ok(resolved) = destination::resolve_variables(uri, dest.project.as_deref(), vars)
        else {
            continue;
        };
        let resolved = std::path::PathBuf::from(resolved);
        let resolved = if resolved.is_absolute() {
            resolved
        } else {
            dest.table_dir.join(resolved)
        };
        let matches = resolved == *target
            || (target.exists() && resolved.canonicalize().ok() == target.canonicalize().ok());
        if !matches || entry.field("type").and_then(Node::value) != Some("KiCad") {
            continue;
        }
        let wanted = dest.nickname();
        if name == wanted
            || name
                .strip_prefix(wanted)
                .is_some_and(|s| s.starts_with('_'))
        {
            changes.extend(
                history
                    .iter()
                    .filter(|(old, new)| *new == &name && !occupied.contains_key(*old))
                    .map(|(old, new)| (old.clone(), new.clone())),
            );
            continue;
        }
        if occupied.contains_key(wanted) {
            bail!(
                "Nickname {wanted} is occupied in {}; resolve it before migration",
                path.display()
            );
        }
        occupied.insert(wanted.into(), ());
        entry.field_mut("name").unwrap().children_mut()[1] = Node::string(wanted);
        history.insert(name.clone(), wanted.into());
        changes.insert(name, wanted.into());
    }
    if history != original_history {
        plan.put(history_path, serde_json::to_vec_pretty(&history)?)?;
    }
    if node != parse(&original)? {
        plan.put(path, format!("{node}\n").into_bytes())?;
    }
    Ok(changes)
}

fn rename(value: &mut Node, mapping: &Renames) {
    if let Node::Atom(s) | Node::Str(s) = value
        && let Some((nick, item)) = s.split_once(':')
        && let Some(new) = mapping.get(nick)
    {
        *s = format!("{new}:{item}");
    }
}
fn references(node: &mut Node, symbols: &Renames, footprints: &Renames, parent: Option<&str>) {
    let head = node.head().map(str::to_owned);
    if let Node::List(items) = node {
        match head.as_deref() {
            Some("lib_id") | Some("extends") => {
                if let Some(v) = items.get_mut(1) {
                    rename(v, symbols);
                }
            }
            Some("symbol") if parent == Some("lib_symbols") => {
                if let Some(v) = items.get_mut(1) {
                    rename(v, symbols);
                }
            }
            Some("footprint") => {
                if let Some(v) = items.get_mut(1) {
                    rename(v, footprints);
                }
            }
            Some("property") if items.get(1).and_then(Node::atom) == Some("Footprint") => {
                if let Some(v) = items.get_mut(2) {
                    rename(v, footprints);
                }
            }
            _ => {}
        }
        for child in items {
            references(child, symbols, footprints, head.as_deref());
        }
    }
}
fn patch(path: &Path, symbols: &Renames, footprints: &Renames, plan: &mut Plan) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let original = fs::read_to_string(path).with_context(|| format!("Read {}", path.display()))?;
    let mut node = parse(&original).with_context(|| format!("Parse {}", path.display()))?;
    let before = node.clone();
    references(&mut node, symbols, footprints, None);
    if node != before {
        plan.put(path.to_owned(), format!("{node}\n").into_bytes())?;
    }
    Ok(())
}
// A local entry shadows the global mapping even if it belongs to another tool.
fn overlay(dest: &Destination, file: &str, global: &Renames, local: Renames) -> Result<Renames> {
    let mut result = global.clone();
    let path = dest.table_dir.join(file);
    if path.exists() {
        for entry in parse(&fs::read_to_string(path)?)?.children() {
            if entry.head() == Some("lib")
                && let Some(name) = entry.field("name").and_then(Node::value)
            {
                result.remove(name);
            }
        }
    }
    result.extend(local);
    Ok(result)
}

pub fn run(cfg: &Config, project: Option<&Path>, fix: bool) -> Result<()> {
    let _lock = Lock::acquire()?;
    let kicad = importer::kicad_config(cfg, project.is_none())?;
    let global = Destination::new(&cfg.libraries.path, None, &kicad)?;
    let mut plan = Plan::default();
    let sym = table(
        &global,
        "sym_lib_table",
        &cfg.libraries.variables,
        &mut plan,
    )?;
    let fp = table(&global, "fp_lib_table", &cfg.libraries.variables, &mut plan)?;
    patch(&global.symbol_path, &sym, &fp, &mut plan)?;
    if let Some(project) = project {
        let local = Destination::new(&cfg.libraries.path, Some(project), &kicad)?;
        let local_sym = table(&local, "sym_lib_table", &cfg.libraries.variables, &mut plan)?;
        let local_fp = table(&local, "fp_lib_table", &cfg.libraries.variables, &mut plan)?;
        let sym = overlay(&local, "sym-lib-table", &sym, local_sym)?;
        let fp = overlay(&local, "fp-lib-table", &fp, local_fp)?;
        patch(&local.symbol_path, &sym, &fp, &mut plan)?;
        for entry in walkdir::WalkDir::new(local.project.as_ref().unwrap()).follow_links(false) {
            let entry = entry?;
            if entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|e| e == "kicad_sch" || e == "kicad_pcb")
            {
                patch(entry.path(), &sym, &fp, &mut plan)?;
            }
        }
    }
    for path in plan.writes.keys() {
        println!(
            "{}: {}",
            if fix { "Repair" } else { "Would repair" },
            path.display()
        );
    }
    if plan.writes.is_empty() {
        println!("No nickname repairs needed.");
    } else if fix {
        let mut record = Record::new(
            global.root.clone(),
            crate::storage::hash(b"doctor-nicknames"),
        );
        record.id = format!(
            "doctor-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        record.status = "importing".into();
        plan.commit(&mut record)?;
        record.status = "completed".into();
        record.message = "Library nickname migration completed".into();
        record.save()?;
        for backup in record.backups {
            println!("Backup: {}", backup.display());
        }
    } else {
        println!("Preview only; rerun with --fix to apply repairs.");
    }
    println!(
        "Run doctor --project /path/to/Board.kicad_pro for each existing project. Close KiCad and stop the watcher before --fix; reopen KiCad afterwards."
    );
    Ok(())
}
