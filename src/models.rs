use crate::{
    destination::{Destination, resolve_variables},
    sexpr::Node,
};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

#[derive(Debug, Default)]
pub struct ModelPlan {
    pub copies: Vec<(PathBuf, Vec<u8>)>,
    pub messages: Vec<String>,
}

fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "step" | "stp" | "wrl"))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn reference_parts(reference: &str) -> Vec<String> {
    reference
        .replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .map(|s| s.to_ascii_lowercase())
        .collect()
}
fn suffix_score(path: &Path, reference: &[String]) -> usize {
    let parts = reference_parts(&path.to_string_lossy());
    parts
        .iter()
        .rev()
        .zip(reference.iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
}

// KIPRJMOD belongs to the footprint's source project when copying a
// registered project library into another project. The target project is only
// the fallback for fresh bundle imports without discoverable source context.
fn source_project(source: &Path) -> Option<PathBuf> {
    for directory in source.parent()?.ancestors() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        if entries.filter_map(Result::ok).any(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|ext| ext == "kicad_pro")
                && entry.path().is_file()
        }) {
            return Some(directory.to_path_buf());
        }
    }
    None
}

/// Resolve an original path first; otherwise rank recursive bundle/search-root
/// matches by trailing path components. Equal-content candidates collapse safely.
pub fn model_candidates(
    reference: &str,
    source_footprint: &Path,
    source_root: &Path,
    search_roots: &[PathBuf],
    variables: &BTreeMap<String, String>,
    project: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    let source_project = source_project(source_footprint);
    let project = source_project.as_deref().or(project);
    let normalized = reference.replace('\\', "/");
    if let Ok(expanded) = resolve_variables(&normalized, project, variables) {
        let path = PathBuf::from(&expanded);
        let direct = if path.is_absolute() {
            path
        } else {
            source_footprint.parent().unwrap_or(source_root).join(path)
        };
        if direct.is_file() && supported(&direct) {
            return Ok(vec![direct]);
        }
        let bundled = source_root.join(expanded.trim_start_matches('/'));
        if bundled.is_file() && supported(&bundled) {
            return Ok(vec![bundled]);
        }
    }
    let parts = reference_parts(&normalized);
    let Some(filename) = parts.last() else {
        return Ok(vec![]);
    };
    let mut roots = vec![source_root.to_path_buf()];
    roots.extend_from_slice(search_roots);
    let mut candidates = BTreeSet::new();
    let mut best_score = 0;
    for root in roots {
        if !root.exists() {
            continue;
        }
        if root.is_file() {
            if supported(&root)
                && root
                    .file_name()
                    .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(filename))
            {
                let score = suffix_score(&root, &parts);
                if score > best_score {
                    candidates.clear();
                    best_score = score;
                }
                if score == best_score {
                    candidates.insert(root.canonicalize()?);
                }
            }
            continue;
        }
        for entry in WalkDir::new(&root).follow_links(false) {
            let entry =
                entry.with_context(|| format!("Search model files in {}", root.display()))?;
            if !entry.file_type().is_file() || !supported(entry.path()) {
                continue;
            }
            if !entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(filename)
            {
                continue;
            }
            let score = suffix_score(entry.path(), &parts);
            if score > best_score {
                candidates.clear();
                best_score = score;
            }
            if score == best_score {
                candidates.insert(entry.path().canonicalize()?);
            }
        }
    }
    let mut unique = BTreeMap::new();
    for candidate in candidates {
        let bytes = std::fs::read(&candidate)
            .with_context(|| format!("Read model {}", candidate.display()))?;
        unique.entry(digest(&bytes)).or_insert(candidate);
    }
    let mut paths: Vec<_> = unique.into_values().collect();
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
pub fn repair_models(
    footprint: &mut Node,
    source_footprint: &Path,
    source_root: &Path,
    destination: &Destination,
    search_roots: &[PathBuf],
    variables: &BTreeMap<String, String>,
    enabled: bool,
) -> Result<ModelPlan> {
    repair_models_with_choices(
        footprint,
        source_footprint,
        source_root,
        destination,
        search_roots,
        variables,
        enabled,
        &BTreeMap::new(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn repair_models_with_choices(
    footprint: &mut Node,
    source_footprint: &Path,
    source_root: &Path,
    destination: &Destination,
    search_roots: &[PathBuf],
    variables: &BTreeMap<String, String>,
    enabled: bool,
    choices: &BTreeMap<String, PathBuf>,
) -> Result<ModelPlan> {
    let mut plan = ModelPlan::default();
    repair_node(
        footprint,
        source_footprint,
        source_root,
        destination,
        search_roots,
        variables,
        enabled,
        choices,
        &mut plan,
    )?;
    Ok(plan)
}

#[allow(clippy::too_many_arguments)]
fn repair_node(
    node: &mut Node,
    source_footprint: &Path,
    source_root: &Path,
    destination: &Destination,
    search_roots: &[PathBuf],
    variables: &BTreeMap<String, String>,
    enabled: bool,
    choices: &BTreeMap<String, PathBuf>,
    plan: &mut ModelPlan,
) -> Result<()> {
    let Node::List(items) = node else {
        return Ok(());
    };
    if matches!(items.first(), Some(Node::Atom(head)) if head == "model") {
        let reference = match items.get(1) {
            Some(Node::Str(s) | Node::Atom(s)) => s.clone(),
            _ => bail!(
                "Model reference missing a filename in {}",
                source_footprint.display()
            ),
        };
        if reference.starts_with("embedded://")
            || reference.starts_with("kicad-embed://")
            || reference.starts_with("${KICAD_EMBEDDED}")
        {
            plan.messages
                .push(format!("Preserved embedded model: {reference}"));
            return Ok(());
        }
        if !enabled {
            plan.messages.push(format!("Model import disabled; external dependency remains unresolved for portability: {reference}"));
            return Ok(());
        }
        let candidates = model_candidates(
            &reference,
            source_footprint,
            source_root,
            search_roots,
            variables,
            destination.project.as_deref(),
        )?;
        let source = if let Some(choice) = choices.get(&reference) {
            let canonical = choice
                .canonicalize()
                .with_context(|| format!("Chosen model is unavailable: {}", choice.display()))?;
            if !candidates
                .iter()
                .any(|candidate| candidate.canonicalize().ok().as_ref() == Some(&canonical))
            {
                bail!(
                    "pending: chosen model is no longer a matching candidate: {}",
                    choice.display()
                );
            }
            canonical
        } else {
            match candidates.as_slice() {
                [] => {
                    plan.messages.push(format!("Missing model; dependency unresolved, original reference preserved: {reference}"));
                    return Ok(());
                }
                [one] => one.clone(),
                many => bail!(
                    "pending: ambiguous model {reference}; choose one of: {}",
                    many.iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" | ")
                ),
            }
        };
        let bytes =
            std::fs::read(&source).with_context(|| format!("Read model {}", source.display()))?;
        // A footprint already in this destination must not acquire another hash
        // suffix each time its models are reviewed or the component is reimported.
        if source.parent().and_then(|p| p.canonicalize().ok())
            == destination.models_dir.canonicalize().ok()
            && destination.models_dir.is_dir()
        {
            items[1] = Node::Str(destination.model_uri(&source));
            plan.messages
                .push(format!("Reused model: {}", source.display()));
            return Ok(());
        }
        let hash = digest(&bytes);
        let stem = source.file_stem().unwrap_or_default().to_string_lossy();
        let safe_stem: String = stem
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let extension = source
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let mut target =
            destination
                .models_dir
                .join(format!("{safe_stem}_{}.{}", &hash[..12], extension));
        if target.exists()
            && std::fs::read(&target)
                .with_context(|| format!("Read existing model {}", target.display()))?
                != bytes
        {
            target = destination
                .models_dir
                .join(format!("{safe_stem}_{hash}.{extension}"));
            if target.exists() && std::fs::read(&target)? != bytes {
                bail!("Model storage collision at {}", target.display());
            }
        }
        let uri = destination.model_uri(&target);
        let reused = target.is_file();
        if !reused && !plan.copies.iter().any(|(path, _)| path == &target) {
            plan.copies.push((target.clone(), bytes));
        }
        items[1] = Node::Str(uri.clone());
        plan.messages.push(format!(
            "{} model: {} -> {}{}",
            if reused { "Reused" } else { "Copied" },
            source.display(),
            uri,
            if reference != source.to_string_lossy() {
                " (reference repaired)"
            } else {
                ""
            }
        ));
        return Ok(());
    }
    for child in items {
        repair_node(
            child,
            source_footprint,
            source_root,
            destination,
            search_roots,
            variables,
            enabled,
            choices,
            plan,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sexpr::parse;
    fn scratch() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "uniloader-models-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn repairs_windows_paths_preserving_transforms() {
        let root = scratch();
        std::fs::create_dir_all(root.join("bundle/3d")).unwrap();
        std::fs::write(root.join("bundle/3d/part.STEP"), b"model").unwrap();
        std::fs::write(root.join("board.kicad_pro"), b"{}").unwrap();
        let dest = Destination::new(
            &root.join("global"),
            Some(&root.join("board.kicad_pro")),
            &root.join("config"),
        )
        .unwrap();
        let mut fp = parse(r#"(footprint "x" (model "C:\\old\\3d\\part.STEP" (offset (xyz 1 2 3)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 90))))"#).unwrap();
        let plan = repair_models(
            &mut fp,
            &root.join("bundle/x.kicad_mod"),
            &root.join("bundle"),
            &dest,
            &[],
            &BTreeMap::new(),
            true,
        )
        .unwrap();
        assert_eq!(plan.copies.len(), 1);
        let rendered = fp.to_string();
        assert!(rendered.contains("${KIPRJMOD}/libraries/models/part_"));
        assert!(rendered.contains("(xyz 0 0 90)"));
        assert!(rendered.contains("(xyz 1 2 3)"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn collapses_identical_candidates_but_requires_choice_for_different_content() {
        let root = scratch();
        for dir in ["a", "b"] {
            std::fs::create_dir(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("part.step"), b"same").unwrap();
        }
        let candidates = model_candidates(
            "C:/obsolete/part.step",
            &root.join("x.kicad_mod"),
            &root,
            &[],
            &BTreeMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(candidates.len(), 1);
        std::fs::write(root.join("b/part.step"), b"different").unwrap();
        assert_eq!(
            model_candidates(
                "C:/obsolete/part.step",
                &root.join("x.kicad_mod"),
                &root,
                &[],
                &BTreeMap::new(),
                None
            )
            .unwrap()
            .len(),
            2
        );
        let dest = Destination::new(&root.join("global"), None, &root.join("config")).unwrap();
        let mut fp = parse(r#"(footprint "x" (model "C:/obsolete/part.step"))"#).unwrap();
        assert!(
            repair_models(
                &mut fp,
                &root.join("x.kicad_mod"),
                &root,
                &dest,
                &[],
                &BTreeMap::new(),
                true
            )
            .unwrap_err()
            .to_string()
            .starts_with("pending:")
        );
        let choices = BTreeMap::from([("C:/obsolete/part.step".into(), root.join("b/part.step"))]);
        let plan = repair_models_with_choices(
            &mut fp,
            &root.join("x.kicad_mod"),
            &root,
            &dest,
            &[],
            &BTreeMap::new(),
            true,
            &choices,
        )
        .unwrap();
        assert_eq!(plan.copies[0].1, b"different");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn external_project_model_uses_source_context_and_becomes_portable() {
        let root = scratch();
        let source = root.join("source");
        let target = root.join("target");
        for project in [&source, &target] {
            std::fs::create_dir_all(project.join("libraries/models")).unwrap();
            std::fs::create_dir_all(project.join("libraries/Parts.pretty")).unwrap();
            std::fs::write(project.join("board.kicad_pro"), b"{}").unwrap();
        }
        std::fs::write(source.join("libraries/models/chip.step"), b"SOURCE MODEL").unwrap();
        // Same URI in another project is not the same dependency.
        std::fs::write(
            target.join("libraries/models/chip.step"),
            b"WRONG TARGET MODEL",
        )
        .unwrap();
        let dest = Destination::new(
            &root.join("global"),
            Some(&target.join("board.kicad_pro")),
            &root.join("config"),
        )
        .unwrap();
        let mut fp = parse(r#"(footprint "chip" (model "${KIPRJMOD}/libraries/models/chip.step" (offset (xyz 1 2 3))))"#).unwrap();
        let source_fp = source.join("libraries/Parts.pretty/chip.kicad_mod");
        let plan = repair_models(
            &mut fp,
            &source_fp,
            source_fp.parent().unwrap(),
            &dest,
            &[],
            &BTreeMap::new(),
            true,
        )
        .unwrap();
        assert_eq!(plan.copies.len(), 1);
        assert_eq!(plan.copies[0].1, b"SOURCE MODEL");
        assert!(plan.copies[0].0.starts_with(&target));
        assert!(
            fp.to_string()
                .contains("${KIPRJMOD}/libraries/models/chip_")
        );
        assert!(fp.to_string().contains("(xyz 1 2 3)"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn conflicting_model_storage_keeps_existing_bytes() {
        let root = scratch();
        std::fs::write(root.join("chip.step"), b"NEW MODEL").unwrap();
        let dest = Destination::new(&root.join("global"), None, &root.join("config")).unwrap();
        std::fs::create_dir_all(&dest.models_dir).unwrap();
        let hash = digest(b"NEW MODEL");
        let collision = dest.models_dir.join(format!("chip_{}.step", &hash[..12]));
        std::fs::write(&collision, b"EXISTING USER MODEL").unwrap();
        let mut fp = parse(r#"(footprint "chip" (model "chip.step"))"#).unwrap();
        let plan = repair_models(
            &mut fp,
            &root.join("chip.kicad_mod"),
            &root,
            &dest,
            &[],
            &BTreeMap::new(),
            true,
        )
        .unwrap();
        assert_ne!(plan.copies[0].0, collision);
        assert_eq!(std::fs::read(&collision).unwrap(), b"EXISTING USER MODEL");
        assert_eq!(plan.copies[0].1, b"NEW MODEL");
        // A second collision refuses to plan an overwrite.
        std::fs::write(&plan.copies[0].0, b"ANOTHER EXISTING MODEL").unwrap();
        let mut original = parse(r#"(footprint "chip" (model "chip.step"))"#).unwrap();
        assert!(
            repair_models(
                &mut original,
                &root.join("chip.kicad_mod"),
                &root,
                &dest,
                &[],
                &BTreeMap::new(),
                true
            )
            .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn embedded_references_are_preserved_without_warnings() {
        let root = scratch();
        let dest = Destination::new(&root.join("global"), None, &root.join("config")).unwrap();
        for reference in [
            "embedded://chip.step",
            "kicad-embed://chip.step",
            "${KICAD_EMBEDDED}/chip.step",
        ] {
            let mut fp = Node::list(vec![
                Node::symbol("footprint"),
                Node::string("chip"),
                Node::list(vec![Node::symbol("model"), Node::string(reference)]),
            ]);
            let before = fp.to_string();
            let plan = repair_models(
                &mut fp,
                &root.join("chip.kicad_mod"),
                &root,
                &dest,
                &[],
                &BTreeMap::new(),
                true,
            )
            .unwrap();
            assert_eq!(fp.to_string(), before);
            assert!(plan.copies.is_empty());
            assert!(plan.messages[0].contains("Preserved embedded"));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn missing_models_remain_visible() {
        let root = scratch();
        let dest = Destination::new(&root.join("global"), None, &root.join("config")).unwrap();
        let mut fp = parse(r#"(footprint "x" (model "missing.wrl"))"#).unwrap();
        let before = fp.to_string();
        let plan = repair_models(
            &mut fp,
            &root.join("x.kicad_mod"),
            &root,
            &dest,
            &[],
            &BTreeMap::new(),
            true,
        )
        .unwrap();
        assert_eq!(fp.to_string(), before);
        assert!(plan.copies.is_empty());
        assert!(plan.messages[0].contains("Missing model"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
