//! Backed-up repair and health checks for managed KiCad libraries.
mod checks;
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
    let original = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        format!("({kind} (version 7))")
    };
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
    let mut managed = false;
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
        let uri = entry
            .field("uri")
            .and_then(Node::value)
            .context("Library entry missing URI")?;
        let resolved = destination::resolve_variables(uri, dest.project.as_deref(), vars)
            .ok()
            .map(std::path::PathBuf::from)
            .map(|p| {
                if p.is_absolute() {
                    p
                } else {
                    dest.table_dir.join(p)
                }
            });
        let matches = resolved.as_ref().is_some_and(|p| {
            p == target || (target.exists() && p.canonicalize().ok() == target.canonicalize().ok())
        });
        // A stale path can be rebuilt only with explicit UniLoader ownership.
        let owned = entry.field("descr").and_then(Node::value) == Some("Imported by UniLoader")
            && (name == "UniLoader" || name.starts_with("UniLoader_"));
        if !matches && !owned {
            continue;
        }
        if managed {
            bail!(
                "Multiple managed registrations in {}; resolve duplicate aliases before repair",
                path.display()
            );
        }
        managed = true;
        let new_uri = if kind == "sym_lib_table" {
            dest.symbol_uri()
        } else {
            dest.footprint_uri()
        };
        entry.field_mut("uri").unwrap().children_mut()[1] = Node::string(new_uri);
        if let Some(field) = entry.field_mut("type") {
            *field = parse("(type \"KiCad\")")?;
        } else {
            entry.children_mut().push(parse("(type \"KiCad\")")?);
        }
        entry
            .children_mut()
            .retain(|n| n.head() != Some("disabled"));
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
    if !managed {
        if occupied.contains_key(dest.nickname()) {
            bail!(
                "Nickname {} is occupied in {}; refusing to repoint an unrelated library",
                dest.nickname(),
                path.display()
            );
        }
        let uri = if kind == "sym_lib_table" {
            dest.symbol_uri()
        } else {
            dest.footprint_uri()
        };
        node = parse(
            &destination::register_table(Some(&node.to_string()), kind, dest.nickname(), &uri)?.0,
        )?;
        changes.extend(
            history
                .iter()
                .filter(|(old, new)| {
                    new.as_str() == dest.nickname() && !occupied.contains_key(*old)
                })
                .map(|(old, new)| (old.clone(), new.clone())),
        );
    }
    if history != original_history {
        plan.put(history_path, serde_json::to_vec_pretty(&history)?)?;
    }
    if !path.exists() || node != parse(&original)? {
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
    let Some(original) = checks::text(path, plan)? else {
        return Ok(());
    };
    let mut node = parse(&original).with_context(|| format!("Parse {}", path.display()))?;
    let before = node.clone();
    references(&mut node, symbols, footprints, None);
    if node != before {
        plan.writes
            .insert(path.to_owned(), format!("{node}\n").into_bytes());
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
    let kicad = importer::kicad_config(cfg, true)?;
    let vars = importer::variables(cfg, &kicad);
    let global = Destination::new(&cfg.libraries.path, None, &kicad)?;
    let mut plan = Plan::default();
    let mut issues = Vec::new();
    checks::structure(&global, &mut plan)?;
    let sym = table(&global, "sym_lib_table", &vars, &mut plan)?;
    let fp = table(&global, "fp_lib_table", &vars, &mut plan)?;
    patch(&global.symbol_path, &sym, &fp, &mut plan)?;
    checks::footprints(cfg, &global, &vars, &mut plan, &mut issues)?;
    let mut local_destination = None;
    if let Some(project) = project {
        let local = Destination::new(&cfg.libraries.path, Some(project), &kicad)?;
        checks::structure(&local, &mut plan)?;
        let local_sym = table(&local, "sym_lib_table", &vars, &mut plan)?;
        let local_fp = table(&local, "fp_lib_table", &vars, &mut plan)?;
        let sym = overlay(&local, "sym-lib-table", &sym, local_sym)?;
        let fp = overlay(&local, "fp-lib-table", &fp, local_fp)?;
        patch(&local.symbol_path, &sym, &fp, &mut plan)?;
        checks::footprints(cfg, &local, &vars, &mut plan, &mut issues)?;
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
        local_destination = Some(local);
    }
    checks::audit(
        &global,
        local_destination.as_ref(),
        &vars,
        &plan,
        &mut issues,
    )?;
    for (label, paths) in [
        ("footprint search", &cfg.libraries.search_paths),
        ("model search", &cfg.libraries.model_search_paths),
    ] {
        for path in paths {
            if !path.exists() {
                issues.push(format!(
                    "Missing configured {label} path: {}",
                    path.display()
                ));
            }
        }
    }
    for directory in &plan.directories {
        println!(
            "{} directory: {}",
            if fix { "Create" } else { "Would create" },
            directory.display()
        );
    }
    for message in &plan.messages {
        println!("{message}");
    }
    for path in plan.writes.keys() {
        println!(
            "{}: {}",
            if fix { "Repair" } else { "Would repair" },
            path.display()
        );
    }
    issues.sort();
    issues.dedup();
    if plan.writes.is_empty() && plan.directories.is_empty() {
        println!("No automatic repairs needed.");
    } else if fix {
        let mut record = Record::new(global.root.clone(), crate::storage::hash(b"doctor-health"));
        record.id = format!(
            "doctor-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        record.status = "importing".into();
        plan.commit(&mut record)?;
        record.status = "completed".into();
        record.message = format!(
            "Doctor repairs completed; {} unresolved issue(s)",
            issues.len()
        );
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
    for issue in &issues {
        eprintln!("Unresolved: {issue}");
    }
    if !issues.is_empty() {
        bail!(
            "Doctor found {} unresolved issue(s); safe repairs {}. Restore deleted components from backups or reimport with --force where needed",
            issues.len(),
            if fix { "applied" } else { "previewed only" }
        );
    }
    println!("No unresolved issues found in the checked scope.");
    Ok(())
}
