use crate::{
    config::{self, Config},
    desktop,
    destination::{self, Destination},
    matching, models,
    sexpr::{self, Node},
    sources,
    storage::{self, Plan, Record},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Default, Clone)]
pub struct Request {
    pub global: bool,
    pub project: Option<PathBuf>,
    pub dry_run: bool,
    pub noninteractive: bool,
    pub force: bool,
}
#[derive(Default)]
pub struct Session {
    pub remembered: Option<(Option<PathBuf>, Vec<PathBuf>)>,
}
struct Foot {
    path: PathBuf,
    node: Node,
    nickname: Option<String>,
    local: bool,
    fingerprint: Option<String>,
}
impl Foot {
    fn name(&self) -> &str {
        self.path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| self.node.value().unwrap_or(""))
    }
    fn fingerprint(&self) -> String {
        self.fingerprint
            .clone()
            .unwrap_or_else(|| matching::exact_fingerprint(&self.node, &self.path))
    }
}
#[derive(Serialize, Deserialize)]
struct Decision {
    path: PathBuf,
    fingerprint: String,
}

fn text(path: &Path) -> Result<Option<String>> {
    if path.exists() {
        Ok(Some(fs::read_to_string(path)?))
    } else {
        Ok(None)
    }
}
fn encoded(n: &Node) -> Vec<u8> {
    format!("{n}\n").into_bytes()
}
fn safe_name(name: &str) -> Result<String> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':', '\0']) {
        bail!("invalid KiCad asset name: {name:?}");
    }
    Ok(name.into())
}

pub fn kicad_config(cfg: &Config, required: bool) -> Result<PathBuf> {
    if let Some(p) = &cfg.libraries.kicad_config {
        return Ok(p.clone());
    }
    let root = config::xdg("XDG_CONFIG_HOME", ".config").join("kicad");
    let mut dirs = vec![];
    if root.exists() {
        for e in fs::read_dir(&root)? {
            let p = e?.path();
            if p.is_dir()
                && p.file_name().is_some_and(|s| {
                    s.to_string_lossy()
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '.')
                })
            {
                dirs.push(p);
            }
        }
    }
    if dirs.len() == 1 {
        return Ok(dirs.remove(0));
    }
    if required {
        bail!(
            "pending: set libraries.kicad_config to the KiCad version directory (none or multiple detected)"
        );
    }
    // No global table to read. This placeholder is never created in project mode.
    Ok(root.join("unselected-version"))
}
fn target(
    cfg: &Config,
    req: &Request,
    record: &Record,
    session: &mut Session,
) -> Result<Option<PathBuf>> {
    if req.global {
        return Ok(None);
    }
    if let Some(p) = &req.project {
        return Ok(Some(config::expand(p)));
    }
    if record.destination.is_some() {
        return Ok(record.project.clone());
    }
    match cfg.destination.policy.as_str() {
        "global" => Ok(None),
        "project" => {
            Ok(Some(cfg.destination.project.clone().context(
                "destination.project must be set for project policy",
            )?))
        }
        _ => {
            let projects = desktop::projects();
            if let Some((remembered, original)) = &session.remembered {
                if original == &projects
                    && (remembered.is_none()
                        || (projects.len() == 1 && projects.first() == remembered.as_ref()))
                {
                    return Ok(remembered.clone());
                }
                session.remembered = None;
            }
            if req.noninteractive || !cfg.ui.enabled {
                bail!("pending: destination requires a desktop choice; use --global or --project");
            }
            let choice = desktop::destination(
                &projects,
                &record
                    .source
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
            )?
            .context("pending: destination choice postponed")?;
            if choice.skip {
                bail!("skipped: destination import skipped");
            }
            if choice.remember && !req.dry_run {
                session.remembered = Some((choice.project.clone(), projects));
            }
            Ok(choice.project)
        }
    }
}
fn variables(cfg: &Config, kicad: &Path) -> BTreeMap<String, String> {
    let mut vars = cfg.libraries.variables.clone();
    if let Some(version) = kicad
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(|s| s.split('.').next())
    {
        for (suffix, path) in [
            ("FOOTPRINT_DIR", "/usr/share/kicad/footprints"),
            ("3DMODEL_DIR", "/usr/share/kicad/3dmodels"),
            ("SYMBOL_DIR", "/usr/share/kicad/symbols"),
        ] {
            if Path::new(path).exists()
                && std::env::var_os(format!("KICAD{version}_{suffix}")).is_none()
            {
                vars.entry(format!("KICAD{version}_{suffix}"))
                    .or_insert_with(|| path.into());
            }
        }
    }
    vars
}
fn index(
    cfg: &Config,
    dest: &Destination,
    kicad: &Path,
    vars: &BTreeMap<String, String>,
    owned_nickname: &str,
) -> Result<Vec<Foot>> {
    let global_table = kicad.join("fp-lib-table");
    let local_table = dest.table_dir.join("fp-lib-table");
    let libs = destination::effective_footprint_libraries(
        &global_table,
        dest.project.as_ref().map(|_| local_table.as_path()),
        dest.project.as_deref(),
        vars,
    )?;
    let mut out = vec![];
    let mut seen = BTreeSet::new();
    for lib in libs {
        let nick = lib.nickname;
        let path = lib.path;
        if !path.is_dir() {
            bail!(
                "pending: registered footprint library {} does not exist",
                path.display()
            );
        }
        for e in fs::read_dir(&path)? {
            let p = e?.path();
            if p.extension().is_some_and(|e| e == "kicad_mod") {
                seen.insert(p.clone());
                out.push(Foot {
                    node: sexpr::parse(&fs::read_to_string(&p)?)?,
                    local: dest.project.as_ref().is_some_and(|r| p.starts_with(r)),
                    path: p,
                    nickname: Some(nick.clone()),
                    fingerprint: None,
                });
            }
        }
    }
    // Existing managed/provider libraries and explicit unregistered roots can be
    // copied into the destination, but must never supply an invalid library ID.
    for root in std::iter::once(&cfg.libraries.path)
        .chain(cfg.libraries.search_paths.iter())
        .chain(std::iter::once(&dest.pretty_dir))
    {
        if !root.exists() {
            if cfg.libraries.search_paths.contains(root) {
                bail!(
                    "pending: configured search root {} is missing",
                    root.display()
                );
            }
            continue;
        }
        for e in walkdir::WalkDir::new(root)
            .follow_links(false)
            .sort_by_file_name()
        {
            let e =
                e.with_context(|| format!("pending: reading search root {}", root.display()))?;
            let p = e.path();
            if !e.file_type().is_file()
                || !p.extension().is_some_and(|e| e == "kicad_mod")
                || !seen.insert(p.to_owned())
            {
                continue;
            }
            let own = p.parent() == Some(dest.pretty_dir.as_path());
            out.push(Foot {
                node: sexpr::parse(&fs::read_to_string(p)?)?,
                path: p.to_owned(),
                nickname: (own && cfg.options.footprints).then(|| owned_nickname.into()),
                local: dest.project.as_ref().is_some_and(|r| p.starts_with(r)),
                fingerprint: None,
            });
        }
    }
    out.sort_by(|a, b| b.local.cmp(&a.local).then(a.path.cmp(&b.path)));
    Ok(out)
}

fn model_choices(
    node: &Node,
    path: &Path,
    bundle: &Path,
    cfg: &Config,
    vars: &BTreeMap<String, String>,
    dest: &Destination,
    req: &Request,
) -> Result<BTreeMap<String, PathBuf>> {
    let mut choices = BTreeMap::new();
    if !cfg.options.models {
        return Ok(choices);
    }
    for model in node.children().iter().filter(|n| n.head() == Some("model")) {
        let Some(reference) = model.value() else {
            continue;
        };
        if reference.starts_with("${KICAD_EMBEDDED}") || reference.starts_with("kicad-embed://") {
            continue;
        }
        let candidates = models::model_candidates(
            reference,
            path,
            bundle,
            &cfg.libraries.model_search_paths,
            vars,
            dest.project.as_deref(),
        )?;
        if candidates.len() > 1 {
            if req.noninteractive || !cfg.ui.enabled {
                bail!(
                    "pending: ambiguous 3D model {reference}: {}",
                    candidates
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            let chosen =
                desktop::choose_file(&format!("Select model for {reference}"), &candidates)?
                    .context("pending: model choice postponed")?;
            choices.insert(reference.into(), chosen);
        } else if let Some(p) = candidates.first() {
            choices.insert(reference.into(), p.clone());
        }
    }
    Ok(choices)
}
fn fingerprint(node: &Node, path: &Path, choices: &BTreeMap<String, PathBuf>) -> String {
    let mut n = node.clone();
    for model in n
        .children_mut()
        .iter_mut()
        .filter(|n| n.head() == Some("model"))
    {
        if let Some(p) = model.value().and_then(|s| choices.get(s)) {
            model.children_mut()[1] = Node::string(p.to_string_lossy());
        }
    }
    matching::exact_fingerprint(&n, path)
}
fn unique_footprint(mut node: Node, dest: &Destination, plan: &Plan) -> Result<(Node, PathBuf)> {
    let name = safe_name(node.value().context("footprint missing name")?)?;
    let hash = storage::hash(node.to_string().as_bytes());
    for n in 0..1000 {
        let candidate = if n == 0 {
            name.clone()
        } else if n == 1 {
            format!("{name}_{}", &hash[..8])
        } else {
            format!("{name}_{}_{}", &hash[..8], n)
        };
        node.children_mut()[1] = Node::string(&candidate);
        let path = dest.pretty_dir.join(format!("{candidate}.kicad_mod"));
        let existing = if let Some(v) = plan.writes.get(&path) {
            Some(v.clone())
        } else if path.exists() {
            Some(fs::read(&path)?)
        } else {
            None
        };
        if existing
            .as_ref()
            .is_none_or(|v| sexpr::parse(&String::from_utf8_lossy(v)).ok().as_ref() == Some(&node))
        {
            return Ok((node, path));
        }
    }
    bail!("cannot allocate a footprint filename")
}
fn binding_add(map: &mut BTreeMap<String, BTreeSet<String>>, alias: String, value: String) {
    map.entry(alias).or_default().insert(value);
}

pub fn run(cfg: &Config, req: &Request, record: &mut Record, session: &mut Session) -> Result<()> {
    let _lock = storage::Lock::acquire()?;
    let bundle = sources::load(&record.source)
        .with_context(|| format!("reading {}", record.source.display()))?;
    let project = target(cfg, req, record, session)?;
    let kicad = kicad_config(cfg, project.is_none())?;
    let dest = Destination::new(&cfg.libraries.path, project.as_deref(), &kicad)?;
    let target_id = if let Some(p) = &dest.project {
        format!("project:{}", p.display())
    } else {
        format!("global:{}:{}", dest.root.display(), kicad.display())
    };
    if record
        .destination
        .as_ref()
        .is_some_and(|saved| saved != &target_id)
    {
        bail!(
            "pending: destination configuration changed since this import was reviewed; start a new manual import with the intended destination"
        );
    }
    if !req.force
        && let Some(completed) = storage::records()?.into_iter().find(|r| {
            r.status == "completed"
                && r.hash == record.hash
                && r.destination.as_ref() == Some(&target_id)
        })
    {
        println!("Already imported into {target_id}; use --force to recheck");
        if !req.dry_run && record.id != completed.id {
            let path = config::data_dir()
                .join("imports")
                .join(format!("{}.json", record.id));
            if let Ok(bytes) = fs::read(&path)
                && let Ok(old) = serde_json::from_slice::<Record>(&bytes)
                && old.status != "completed"
            {
                let _ = fs::remove_file(path);
            }
        }
        return Ok(());
    }
    let previous_id = record.id.clone();
    record.id = format!(
        "{}-{}",
        &record.hash[..16],
        &storage::hash(target_id.as_bytes())[..8]
    );
    record.destination = Some(target_id);
    record.project = project.clone();
    if !req.dry_run {
        record.save()?;
        if previous_id != record.id {
            let p = config::data_dir()
                .join("imports")
                .join(format!("{previous_id}.json"));
            if let Ok(bytes) = fs::read(&p)
                && let Ok(old) = serde_json::from_slice::<Record>(&bytes)
                && old.status != "completed"
            {
                let _ = fs::remove_file(p);
            }
        }
    }
    let vars = variables(cfg, &kicad);
    let mut plan = Plan::default();
    plan.messages.push(format!(
        "Source: {} ({})",
        record.source.display(),
        bundle.provider
    ));
    plan.messages
        .push(format!("Destination: {}", dest.root.display()));
    let (fp_table, fp_nickname) = destination::register_table(
        text(&dest.table_dir.join("fp-lib-table"))?.as_deref(),
        "fp_lib_table",
        dest.nickname(),
        &dest.footprint_uri(),
    )?;
    let (sym_table, sym_nickname) = destination::register_table(
        text(&dest.table_dir.join("sym-lib-table"))?.as_deref(),
        "sym_lib_table",
        dest.nickname(),
        &dest.symbol_uri(),
    )?;
    let mut library_index = index(cfg, &dest, &kicad, &vars, &fp_nickname)
        .context("pending: footprint search could not be completed")?;
    let mut bindings: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut decisions = vec![];
    let decision_dir = config::data_dir().join("decisions");
    let mut assets: Vec<(PathBuf, Node, Vec<String>, PathBuf)> = bundle
        .footprints
        .iter()
        .map(|(p, n)| (p.clone(), n.clone(), vec![], bundle.root.clone()))
        .collect();
    // Symbols sometimes reference an already installed footprint without bundling it.
    // Bring that dependency through the same portable-copy/model pipeline.
    if cfg.options.symbols {
        let bundled_aliases: BTreeSet<String> = bundle
            .footprints
            .iter()
            .flat_map(|(p, n)| {
                let parent = p
                    .parent()
                    .and_then(Path::file_stem)
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                [
                    n.value().unwrap_or(""),
                    p.file_stem().and_then(|s| s.to_str()).unwrap_or(""),
                ]
                .into_iter()
                .map(move |name| format!("{parent}:{name}"))
            })
            .collect();
        let mut added = BTreeSet::new();
        for (_, lib) in &bundle.symbols {
            for symbol in lib.children().iter().filter(|n| n.head() == Some("symbol")) {
                for prop in symbol
                    .children()
                    .iter()
                    .filter(|n| n.head() == Some("property") && n.value() == Some("Footprint"))
                {
                    let Some(reference) = prop.children().get(2).and_then(Node::atom) else {
                        continue;
                    };
                    let Some((nick, name)) = reference.split_once(':') else {
                        continue;
                    };
                    if bundled_aliases.contains(reference) || !added.insert(reference.to_owned()) {
                        continue;
                    }
                    if let Some(f) = library_index
                        .iter()
                        .find(|f| f.nickname.as_deref() == Some(nick) && f.name() == name)
                    {
                        assets.push((
                            f.path.clone(),
                            f.node.clone(),
                            vec![reference.to_owned()],
                            f.path.parent().unwrap_or(Path::new(".")).to_owned(),
                        ));
                    }
                }
            }
        }
    }
    for (source, node, extra_aliases, source_root) in &assets {
        let choices = model_choices(node, source, source_root, cfg, &vars, &dest, req)?;
        let incoming_hash = fingerprint(node, source, &choices);
        let exact = if cfg.matching.enabled {
            library_index
                .iter()
                .position(|f| f.fingerprint() == incoming_hash)
        } else {
            None
        };
        let potential: Vec<usize> = if cfg.matching.enabled {
            library_index
                .iter()
                .enumerate()
                .filter(|(_, f)| matching::similar(node, &f.node, cfg.matching.tolerance))
                .map(|(i, _)| i)
                .collect()
        } else {
            vec![]
        };
        let remembered = fs::read(decision_dir.join(format!("{incoming_hash}.json")))
            .ok()
            .and_then(|v| serde_json::from_slice::<Decision>(&v).ok())
            .and_then(|d| {
                library_index
                    .iter()
                    .position(|f| f.path == d.path && f.fingerprint() == d.fingerprint)
            });
        let reuse = if exact.is_some() && cfg.matching.exact == "reuse" {
            exact
        } else if remembered.is_some() {
            remembered
        } else if !potential.is_empty() || exact.is_some() {
            let mut candidates = potential;
            if let Some(i) = exact
                && !candidates.contains(&i)
            {
                candidates.insert(0, i);
            }
            match cfg.matching.ambiguous.as_str() {
                "separate" => None,
                "skip" => bail!(
                    "skipped: footprint {} has uncertain candidates",
                    node.value().unwrap_or("?")
                ),
                _ => {
                    if req.noninteractive || !cfg.ui.enabled {
                        bail!(
                            "pending: footprint {} requires comparison with {} candidates",
                            node.value().unwrap_or("?"),
                            candidates.len()
                        );
                    }
                    if req.dry_run {
                        bail!(
                            "pending: dry run requires footprint comparison for {}",
                            node.value().unwrap_or("?")
                        );
                    }
                    let items = candidates
                        .iter()
                        .map(|i| {
                            let f = &library_index[*i];
                            desktop::Candidate {
                                label: format!(
                                    "{}:{}",
                                    f.nickname.as_deref().unwrap_or("unregistered"),
                                    f.name()
                                ),
                                path: f.path.clone(),
                                raw: f.node.to_string(),
                                differences: matching::differences(node, &f.node),
                            }
                        })
                        .collect::<Vec<_>>();
                    match desktop::compare(&node.to_string(), &items)?
                        .context("pending: footprint comparison postponed")?
                    {
                        desktop::FootprintChoice::Reuse(n) => {
                            let i = *candidates.get(n).context("invalid candidate selection")?;
                            decisions.push((
                                incoming_hash.clone(),
                                Decision {
                                    path: library_index[i].path.clone(),
                                    fingerprint: library_index[i].fingerprint(),
                                },
                            ));
                            Some(i)
                        }
                        desktop::FootprintChoice::Separate => None,
                        desktop::FootprintChoice::Skip => {
                            bail!("skipped: component skipped in footprint comparison")
                        }
                    }
                }
            }
        } else {
            None
        };
        let selected = reuse.map(|i| &library_index[i]);
        if let Some(f) = selected
            && f.path.exists()
            && !plan.writes.contains_key(&f.path)
        {
            let fresh = sexpr::parse(&fs::read_to_string(&f.path)?)?;
            if matching::exact_fingerprint(&fresh, &f.path) != f.fingerprint() {
                bail!(
                    "pending: selected footprint changed during comparison: {}",
                    f.path.display()
                );
            }
        }
        let in_place = selected.is_some_and(|f| {
            f.nickname.is_some() && (dest.project.is_none() || !cfg.destination.portable || f.local)
        });
        let final_id = if in_place {
            let f = selected.unwrap();
            if dest.project.is_some() && cfg.destination.portable {
                let mut repaired = f.node.clone();
                let root = f.path.parent().unwrap_or(Path::new("."));
                let chosen_choices =
                    model_choices(&repaired, &f.path, root, cfg, &vars, &dest, req)?;
                let model_plan = models::repair_models_with_choices(
                    &mut repaired,
                    &f.path,
                    root,
                    &dest,
                    &cfg.libraries.model_search_paths,
                    &vars,
                    cfg.options.models,
                    &chosen_choices,
                )?;
                if repaired != f.node && !cfg.options.footprints {
                    bail!(
                        "pending: footprint import disabled but portable model repair requires updating {}",
                        f.path.display()
                    );
                }
                for (p, bytes) in model_plan.copies {
                    plan.put(p, bytes)?;
                }
                plan.messages.extend(model_plan.messages);
                if repaired != f.node {
                    plan.put(f.path.clone(), encoded(&repaired))?;
                }
            }
            plan.messages
                .push(format!("Reuse footprint {}", f.path.display()));
            format!("{}:{}", f.nickname.as_ref().unwrap(), f.name())
        } else {
            if !cfg.options.footprints {
                bail!(
                    "pending: footprint import disabled and {} has no usable registered match",
                    node.value().unwrap_or("?")
                );
            }
            let (mut chosen, chosen_path, chosen_root) = if let Some(f) = selected {
                (
                    f.node.clone(),
                    f.path.clone(),
                    f.path.parent().unwrap_or(Path::new(".")).to_owned(),
                )
            } else {
                (node.clone(), source.clone(), source_root.clone())
            };
            let chosen_choices = if selected.is_some() {
                model_choices(&chosen, &chosen_path, &chosen_root, cfg, &vars, &dest, req)?
            } else {
                choices
            };
            let chosen_hash = fingerprint(&chosen, &chosen_path, &chosen_choices);
            let model_plan = models::repair_models_with_choices(
                &mut chosen,
                &chosen_path,
                &chosen_root,
                &dest,
                &cfg.libraries.model_search_paths,
                &vars,
                cfg.options.models,
                &chosen_choices,
            )?;
            for (p, bytes) in model_plan.copies {
                plan.put(p, bytes)?;
            }
            plan.messages.extend(model_plan.messages);
            let (chosen, path) = unique_footprint(chosen, &dest, &plan)?;
            let name = chosen.value().unwrap().to_owned();
            plan.messages
                .push(format!("Install footprint {}", path.display()));
            plan.put(path.clone(), encoded(&chosen))?;
            library_index.insert(
                0,
                Foot {
                    path,
                    node: chosen,
                    nickname: Some(fp_nickname.clone()),
                    local: dest.project.is_some(),
                    fingerprint: Some(chosen_hash),
                },
            );
            format!("{fp_nickname}:{name}")
        };
        for alias in extra_aliases {
            binding_add(&mut bindings, alias.clone(), final_id.clone());
        }
        if !extra_aliases.is_empty() {
            continue;
        }
        let original = node.value().context("footprint missing name")?.to_owned();
        binding_add(&mut bindings, original.clone(), final_id.clone());
        if let Some(stem) = source.file_stem().and_then(|s| s.to_str()) {
            binding_add(&mut bindings, stem.into(), final_id.clone());
        }
        if let Some(parent) = source
            .parent()
            .and_then(Path::file_stem)
            .and_then(|s| s.to_str())
        {
            binding_add(&mut bindings, format!("{parent}:{original}"), final_id);
        }
    }
    if cfg.options.footprints && !assets.is_empty() {
        plan.directories.push(dest.pretty_dir.clone());
        plan.put(dest.table_dir.join("fp-lib-table"), fp_table.into_bytes())?;
    }
    if cfg.options.symbols && !bundle.symbols.is_empty() {
        let current = if let Some(raw) = text(&dest.symbol_path)? {
            sexpr::parse(&raw)?
        } else {
            Node::list(vec![
                Node::symbol("kicad_symbol_lib"),
                Node::list(vec![Node::symbol("version"), Node::symbol("20211014")]),
                Node::list(vec![Node::symbol("generator"), Node::string("uniloader")]),
            ])
        };
        if current.head() != Some("kicad_symbol_lib") {
            bail!("target is not a symbol library");
        }
        let mut incoming = vec![];
        for (_, lib) in &bundle.symbols {
            let mut rewritten = lib.clone();
            for symbol in rewritten
                .children_mut()
                .iter_mut()
                .filter(|n| n.head() == Some("symbol"))
            {
                for prop in symbol
                    .children_mut()
                    .iter_mut()
                    .filter(|n| n.head() == Some("property") && n.value() == Some("Footprint"))
                {
                    let original = prop
                        .children()
                        .get(2)
                        .and_then(Node::atom)
                        .unwrap_or("")
                        .to_owned();
                    if original.is_empty() {
                        continue;
                    }
                    let resolved = bindings
                        .get(&original)
                        .or_else(|| bindings.get(original.rsplit(':').next().unwrap_or(&original)));
                    let id = if let Some(set) = resolved {
                        if set.len() != 1 {
                            bail!("pending: ambiguous symbol footprint association {original}");
                        }
                        set.first().unwrap().clone()
                    } else {
                        let Some((libname, fpname)) = original.split_once(':') else {
                            bail!("pending: no footprint found for {original}");
                        };
                        let found = library_index
                            .iter()
                            .find(|f| f.nickname.as_deref() == Some(libname) && f.name() == fpname);
                        let f = found.with_context(|| {
                            format!("pending: missing registered footprint {original}")
                        })?;
                        if dest.project.is_some() && cfg.destination.portable && !f.local {
                            bail!(
                                "pending: symbol references external footprint {original} not bundled; import its footprint into the project first"
                            );
                        }
                        original
                    };
                    prop.children_mut()[2] = Node::string(id);
                }
            }
            incoming.push(rewritten);
        }
        let (merged, messages) = crate::symbols::merge(current, incoming)?;
        plan.messages.extend(messages);
        plan.put(dest.symbol_path.clone(), encoded(&merged))?;
        plan.put(dest.table_dir.join("sym-lib-table"), sym_table.into_bytes())?;
        plan.messages.push(format!(
            "Symbol library {sym_nickname}: {}",
            dest.symbol_path.display()
        ));
    }
    for message in &plan.messages {
        println!("{message}");
    }
    for p in plan.writes.keys() {
        println!(
            "{} {}",
            if req.dry_run { "Would write" } else { "Write" },
            p.display()
        );
    }
    if req.dry_run {
        println!("Dry run: no libraries or decisions written");
        return Ok(());
    }
    record.status = "importing".into();
    record.message = plan.messages.join("\n");
    record.save()?;
    plan.commit(record)?;
    for (key, decision) in decisions {
        fs::create_dir_all(&decision_dir)?;
        storage::atomic(
            &decision_dir.join(format!("{key}.json")),
            &serde_json::to_vec_pretty(&decision)?,
        )?;
    }
    record.status = "completed".into();
    record.message = plan.messages.join("\n");
    record.save()?;
    if !cfg.options.preserve_source && record.source.is_file() {
        fs::remove_file(&record.source).context("import succeeded but source cleanup failed")?;
    }
    println!("Imported successfully. KiCad may need a library refresh/reopen.");
    Ok(())
}
