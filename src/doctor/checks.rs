//! Inspect the staged repair state, so preview and fix run the same checks.
use crate::{
    config::Config,
    destination::{self, Destination},
    models,
    sexpr::{Node, parse},
    storage::Plan,
};
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

type Registry = BTreeMap<String, PathBuf>;
type SymbolCache = BTreeMap<PathBuf, std::collections::BTreeSet<String>>;

pub(super) fn text(path: &Path, plan: &Plan) -> Result<Option<String>> {
    if let Some(bytes) = plan.writes.get(path) {
        return Ok(Some(String::from_utf8(bytes.clone())?));
    }
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Read {}", path.display())),
    }
}

pub(super) fn structure(dest: &Destination, plan: &mut Plan) -> Result<()> {
    for path in [
        &dest.root,
        &dest.table_dir,
        &dest.pretty_dir,
        &dest.models_dir,
    ] {
        for ancestor in path.ancestors() {
            match ancestor.symlink_metadata() {
                Ok(_) if !ancestor.is_dir() => bail!(
                    "Expected a managed directory or directory ancestor (check broken symlinks): {}",
                    ancestor.display()
                ),
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(e)
                        .with_context(|| format!("Inspect directory {}", ancestor.display()));
                }
                _ => {}
            }
        }
        if !path.is_dir() {
            plan.directories.push(path.clone());
        } else {
            fs::read_dir(path).with_context(|| format!("Read directory {}", path.display()))?;
        }
    }
    if dest.symbol_path.symlink_metadata().is_ok() && !dest.symbol_path.is_file() {
        bail!(
            "Expected a managed symbol library file: {}",
            dest.symbol_path.display()
        );
    }
    if !dest.symbol_path.exists() {
        plan.put(
            dest.symbol_path.clone(),
            b"(kicad_symbol_lib (version 20211014) (generator \"uniloader\"))\n".to_vec(),
        )?;
        plan.messages.push(format!("Recreate empty symbol library: {}. Deleted symbols require restore or reimport --force.", dest.symbol_path.display()));
    } else {
        let node = parse(&fs::read_to_string(&dest.symbol_path)?)?;
        if node.head() != Some("kicad_symbol_lib")
            || node
                .field("version")
                .and_then(Node::value)
                .and_then(|v| v.parse::<u64>().ok())
                .is_none()
        {
            bail!(
                "Invalid managed symbol library {}; restore it from backup (not overwritten)",
                dest.symbol_path.display()
            );
        }
    }
    Ok(())
}

pub(super) fn footprints(
    cfg: &Config,
    dest: &Destination,
    vars: &RegistryStrings,
    plan: &mut Plan,
    issues: &mut Vec<String>,
) -> Result<()> {
    if !dest.pretty_dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&dest.pretty_dir)? {
        let path = entry?.path();
        if !path.extension().is_some_and(|e| e == "kicad_mod") {
            continue;
        }
        let original = text(&path, plan)?.context("Missing footprint")?;
        let mut node = parse(&original).with_context(|| format!("Parse {}", path.display()))?;
        if !matches!(node.head(), Some("footprint" | "module")) {
            bail!("Invalid managed footprint {}", path.display());
        }
        let before = node.clone();
        let repaired = models::repair_models_with_choices(
            &mut node,
            &path,
            &dest.root,
            dest,
            &cfg.libraries.model_search_paths,
            vars,
            true,
            &BTreeMap::new(),
        );
        match repaired {
            Ok(models) => {
                for message in models
                    .messages
                    .into_iter()
                    .filter(|m| m.contains("unresolved"))
                {
                    issues.push(format!("{}: {message}", path.display()));
                }
                for (target, bytes) in models.copies {
                    plan.put(target, bytes)?;
                }
                if node != before {
                    plan.writes.insert(path, format!("{node}\n").into_bytes());
                }
            }
            Err(error) => issues.push(format!("{}: {error:#}", path.display())),
        }
    }
    Ok(())
}

type RegistryStrings = BTreeMap<String, String>;
fn registrations(
    dest: &Destination,
    file: &str,
    kind: &str,
    vars: &RegistryStrings,
    plan: &Plan,
    effective: &mut Registry,
    issues: &mut Vec<String>,
) -> Result<()> {
    let path = dest.table_dir.join(file);
    let Some(raw) = text(&path, plan)? else {
        return Ok(());
    };
    let node = parse(&raw)?;
    if node.head() != Some(kind) {
        bail!("Invalid library table {}", path.display());
    }
    for entry in node.children().iter().filter(|n| n.head() == Some("lib")) {
        let name = entry
            .field("name")
            .and_then(Node::value)
            .context("Library entry missing name")?;
        effective.remove(name);
        if entry.field("disabled").is_some() {
            continue;
        }
        if entry.field("type").and_then(Node::value) != Some("KiCad") {
            continue;
        }
        let uri = entry
            .field("uri")
            .and_then(Node::value)
            .context("Library entry missing URI")?;
        match destination::resolve_variables(uri, dest.project.as_deref(), vars) {
            Ok(resolved) => {
                let resolved = PathBuf::from(resolved);
                let resolved = if resolved.is_absolute() {
                    resolved
                } else {
                    dest.table_dir.join(resolved)
                };
                let valid = if kind == "sym_lib_table" {
                    resolved.is_file() || plan.writes.contains_key(&resolved)
                } else {
                    resolved.is_dir() || plan.directories.contains(&resolved)
                };
                if !valid {
                    issues.push(format!("Registered library {name} in {} has missing/wrong-type path: {} (unmanaged paths are not created)", path.display(), resolved.display()));
                } else if kind == "sym_lib_table" && resolved.is_file() {
                    fs::File::open(&resolved)
                        .with_context(|| format!("Read {}", resolved.display()))?;
                } else if kind == "fp_lib_table" && resolved.is_dir() {
                    fs::read_dir(&resolved)
                        .with_context(|| format!("Read {}", resolved.display()))?;
                }
                effective.insert(name.into(), resolved);
            }
            Err(e) => issues.push(format!("Library {name} in {}: {e:#}", path.display())),
        }
    }
    Ok(())
}
fn reference(
    value: &str,
    symbols: bool,
    file: &Path,
    registry: &Registry,
    plan: &Plan,
    issues: &mut Vec<String>,
    cache: &mut SymbolCache,
) -> Result<()> {
    if value.is_empty() {
        return Ok(());
    }
    let Some((nickname, item)) = value.split_once(':') else {
        issues.push(format!(
            "{}: unqualified library reference {value}",
            file.display()
        ));
        return Ok(());
    };
    let Some(library) = registry.get(nickname) else {
        issues.push(format!(
            "{}: reference {value} has no enabled KiCad library registration",
            file.display()
        ));
        return Ok(());
    };
    if item.is_empty() || item.contains(['/', '\\', ':']) || item == "." || item == ".." {
        issues.push(format!(
            "{}: invalid library reference {value}",
            file.display()
        ));
        return Ok(());
    }
    let found = if symbols {
        if !cache.contains_key(library) {
            let mut names = std::collections::BTreeSet::new();
            if let Some(raw) = text(library, plan)? {
                match parse(&raw) {
                    Ok(lib) if lib.head() == Some("kicad_symbol_lib") => {
                        names.extend(
                            lib.children()
                                .iter()
                                .filter(|n| n.head() == Some("symbol"))
                                .filter_map(Node::value)
                                .map(str::to_owned),
                        );
                    }
                    _ => issues.push(format!(
                        "Invalid registered symbol library {}",
                        library.display()
                    )),
                }
            }
            cache.insert(library.clone(), names);
        }
        cache[library].contains(item)
    } else {
        let path = library.join(format!("{item}.kicad_mod"));
        path.is_file() || plan.writes.contains_key(&path)
    };
    if !found {
        issues.push(format!(
            "{}: missing {} {value}; restore or reimport the component",
            file.display(),
            if symbols { "symbol" } else { "footprint" }
        ));
    }
    Ok(())
}
fn links(
    node: &Node,
    file: &Path,
    sym: &Registry,
    fp: &Registry,
    plan: &Plan,
    issues: &mut Vec<String>,
    cache: &mut SymbolCache,
) -> Result<()> {
    match node.head() {
        Some("lib_id") => {
            if let Some(value) = node.value() {
                reference(value, true, file, sym, plan, issues, cache)?;
            }
        }
        Some("footprint") if node.value().is_some_and(|s| s.contains(':')) => {
            reference(node.value().unwrap(), false, file, fp, plan, issues, cache)?;
        }
        Some("property") if node.value() == Some("Footprint") => {
            if let Some(value) = node.children().get(2).and_then(Node::atom) {
                reference(value, false, file, fp, plan, issues, cache)?;
            }
        }
        _ => {}
    }
    for child in node.children() {
        links(child, file, sym, fp, plan, issues, cache)?;
    }
    Ok(())
}
fn inspect(
    path: &Path,
    sym: &Registry,
    fp: &Registry,
    plan: &Plan,
    issues: &mut Vec<String>,
    cache: &mut SymbolCache,
) -> Result<()> {
    if let Some(raw) = text(path, plan)? {
        let node = parse(&raw).with_context(|| format!("Parse {}", path.display()))?;
        let expected = match path.extension().and_then(|e| e.to_str()) {
            Some("kicad_sym") => Some("kicad_symbol_lib"),
            Some("kicad_sch") => Some("kicad_sch"),
            Some("kicad_pcb") => Some("kicad_pcb"),
            _ => None,
        };
        if let Some(expected) = expected
            && node.head() != Some(expected)
        {
            bail!(
                "Invalid KiCad file {}: expected {}",
                path.display(),
                expected
            );
        }
        links(&node, path, sym, fp, plan, issues, cache)?;
        if node.head() == Some("kicad_symbol_lib") {
            let mut names = std::collections::BTreeSet::new();
            for symbol in node
                .children()
                .iter()
                .filter(|n| n.head() == Some("symbol"))
            {
                let name = symbol.value().context("Symbol missing name")?;
                if !names.insert(name) {
                    issues.push(format!("{}: duplicate symbol {name}", path.display()));
                }
            }
            for symbol in node
                .children()
                .iter()
                .filter(|n| n.head() == Some("symbol"))
            {
                if let Some(parent) = symbol.field("extends").and_then(Node::value)
                    && !names.contains(parent)
                {
                    issues.push(format!(
                        "{}: symbol {} extends missing parent {parent}",
                        path.display(),
                        symbol.value().unwrap_or("?")
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn audit(
    global: &Destination,
    local: Option<&Destination>,
    vars: &RegistryStrings,
    plan: &Plan,
    issues: &mut Vec<String>,
) -> Result<()> {
    let mut cache = SymbolCache::new();
    let mut sym = Registry::new();
    let mut fp = Registry::new();
    registrations(
        global,
        "sym-lib-table",
        "sym_lib_table",
        vars,
        plan,
        &mut sym,
        issues,
    )?;
    registrations(
        global,
        "fp-lib-table",
        "fp_lib_table",
        vars,
        plan,
        &mut fp,
        issues,
    )?;
    inspect(&global.symbol_path, &sym, &fp, plan, issues, &mut cache)?;
    if let Some(local) = local {
        registrations(
            local,
            "sym-lib-table",
            "sym_lib_table",
            vars,
            plan,
            &mut sym,
            issues,
        )?;
        registrations(
            local,
            "fp-lib-table",
            "fp_lib_table",
            vars,
            plan,
            &mut fp,
            issues,
        )?;
        inspect(&local.symbol_path, &sym, &fp, plan, issues, &mut cache)?;
        for entry in walkdir::WalkDir::new(local.project.as_ref().unwrap()).follow_links(false) {
            let entry = entry?;
            if entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|e| e == "kicad_sch" || e == "kicad_pcb")
            {
                inspect(entry.path(), &sym, &fp, plan, issues, &mut cache)?;
            }
        }
    }
    Ok(())
}
