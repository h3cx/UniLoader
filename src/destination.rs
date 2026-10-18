use crate::sexpr::{Node, parse};
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Destination {
    pub project: Option<PathBuf>,
    pub root: PathBuf,
    pub table_dir: PathBuf,
    pub symbol_path: PathBuf,
    pub pretty_dir: PathBuf,
    pub models_dir: PathBuf,
}

impl Destination {
    pub fn new(global_root: &Path, project: Option<&Path>, kicad_config: &Path) -> Result<Self> {
        let project = project
            .map(|p| -> Result<PathBuf> {
                if p.extension().and_then(|x| x.to_str()) != Some("kicad_pro") || !p.is_file() {
                    bail!(
                        "Project must be an existing .kicad_pro file: {}",
                        p.display()
                    );
                }
                Ok(p.canonicalize()?
                    .parent()
                    .context("Project has no parent directory")?
                    .to_path_buf())
            })
            .transpose()?;
        let root = match &project {
            Some(p) => p.join("libraries"),
            None => absolute(global_root)?,
        };
        let table_dir = match &project {
            Some(p) => p.clone(),
            None => absolute(kicad_config)?,
        };
        let result = Self {
            project,
            symbol_path: root.join("UniLoader.kicad_sym"),
            pretty_dir: root.join("UniLoader.pretty"),
            models_dir: root.join("models"),
            root,
            table_dir,
        };
        for path in [
            &result.root,
            &result.table_dir,
            &result.pretty_dir,
            &result.models_dir,
        ] {
            if path.exists() && !path.is_dir() {
                bail!("Expected a directory: {}", path.display());
            }
        }
        if result.symbol_path.exists() && !result.symbol_path.is_file() {
            bail!(
                "Expected a symbol library file: {}",
                result.symbol_path.display()
            );
        }
        Ok(result)
    }
    pub fn nickname(&self) -> &'static str {
        if self.project.is_some() {
            "UniLoader_Local"
        } else {
            "UniLoader_Global"
        }
    }
    fn uri(&self, path: &Path) -> String {
        if let Some(project) = &self.project
            && let Ok(relative) = path.strip_prefix(project)
        {
            return format!("${{KIPRJMOD}}/{}", relative.to_string_lossy());
        }
        path.to_string_lossy().into_owned()
    }
    pub fn model_uri(&self, path: &Path) -> String {
        self.uri(path)
    }
    pub fn footprint_uri(&self) -> String {
        self.uri(&self.pretty_dir)
    }
    pub fn symbol_uri(&self) -> String {
        self.uri(&self.symbol_path)
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    let text = path.to_string_lossy();
    let expanded = if text == "~" {
        PathBuf::from(env::var("HOME")?)
    } else if let Some(suffix) = text.strip_prefix("~/") {
        PathBuf::from(env::var("HOME")?).join(suffix)
    } else {
        path.to_path_buf()
    };
    Ok(if expanded.is_absolute() {
        expanded
    } else {
        env::current_dir()?.join(expanded)
    })
}

fn field_value<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    match node {
        Node::List(items) => items.iter().find_map(|item| match item {
            Node::List(parts) if matches!(parts.first(), Some(Node::Atom(head)) if head == name) =>
                parts.get(1).and_then(|p| match p { Node::Atom(s) | Node::Str(s) => Some(s.as_str()), _ => None }),
            _ => None,
        }),
        _ => None,
    }
}
fn field(name: &str, value: &str) -> Node {
    Node::List(vec![Node::Atom(name.into()), Node::Str(value.into())])
}

/// Merge our entry without changing another library's nickname or URI.
pub fn register_table(
    existing: Option<&str>,
    kind: &str,
    requested_nickname: &str,
    uri: &str,
) -> Result<(String, String)> {
    if kind != "sym_lib_table" && kind != "fp_lib_table" {
        bail!("Unsupported library table: {kind}");
    }
    let mut table = match existing {
        Some(text) => parse(text)?,
        None => Node::List(vec![
            Node::Atom(kind.into()),
            Node::List(vec![Node::Atom("version".into()), Node::Atom("7".into())]),
        ]),
    };
    let Node::List(items) = &mut table else {
        bail!("Library table must be a list");
    };
    if !matches!(items.first(), Some(Node::Atom(head)) if head == kind) {
        bail!("Expected {kind} library table");
    }
    // Reuse our existing registration even if a previous collision changed its name.
    if let Some(name) = items.iter().find_map(|entry| {
        (field_value(entry, "uri") == Some(uri) && field_value(entry, "type") == Some("KiCad") && !matches!(entry, Node::List(parts) if parts.iter().any(|part| matches!(part, Node::List(fields) if matches!(fields.first(), Some(Node::Atom(head)) if head == "disabled")))))
            .then(|| field_value(entry, "name").map(str::to_owned))
            .flatten()
    }) {
        if requested_nickname.starts_with("UniLoader_") && !name.starts_with(requested_nickname) {
            bail!("Legacy managed library nickname {name}; run `uniloader doctor --global --fix` or `uniloader doctor --project /path/to/Board.kicad_pro --fix` before importing");
        }
        return Ok((format!("{table}\n"), name));
    }
    let mut nickname = requested_nickname.to_owned();
    let mut suffix = 2;
    while items
        .iter()
        .any(|entry| field_value(entry, "name") == Some(nickname.as_str()))
    {
        nickname = format!("{requested_nickname}_{suffix}");
        suffix += 1;
    }
    items.push(Node::List(vec![
        Node::Atom("lib".into()),
        field("name", &nickname),
        field("type", "KiCad"),
        field("uri", uri),
        field("options", ""),
        field("descr", "Imported by UniLoader"),
    ]));
    Ok((format!("{table}\n"), nickname))
}

#[derive(Debug, Clone)]
pub struct LibraryEntry {
    pub nickname: String,
    pub path: PathBuf,
}

pub fn resolve_variables(
    text: &str,
    project: Option<&Path>,
    variables: &BTreeMap<String, String>,
) -> Result<String> {
    let mut resolved = text.to_owned();
    for _ in 0..16 {
        let Some(start) = resolved.find("${") else {
            return Ok(resolved);
        };
        let end = resolved[start + 2..]
            .find('}')
            .context("Unterminated KiCad path variable")?
            + start
            + 2;
        let name = &resolved[start + 2..end];
        let value = if name == "KIPRJMOD" {
            project
                .context("KIPRJMOD requires a project destination")?
                .to_string_lossy()
                .into_owned()
        } else {
            variables
                .get(name)
                .cloned()
                .or_else(|| env::var(name).ok())
                .with_context(|| format!("Unresolved KiCad path variable: {name}"))?
        };
        resolved.replace_range(start..=end, &value);
    }
    bail!("Recursive or excessive KiCad path variables in {text}")
}

/// Read project overrides before validating paths. A broken global URI must
/// not prevent importing when the project supplies a valid replacement.
pub fn effective_footprint_libraries(
    global: &Path,
    local: Option<&Path>,
    project: Option<&Path>,
    variables: &BTreeMap<String, String>,
) -> Result<Vec<LibraryEntry>> {
    let mut effective = BTreeMap::<String, (Node, PathBuf)>::new();
    for table in std::iter::once(global).chain(local) {
        if !table.exists() {
            continue;
        }
        let node = parse(
            &std::fs::read_to_string(table).with_context(|| format!("Read {}", table.display()))?,
        )?;
        let Node::List(items) = node else {
            bail!("Invalid footprint table {}", table.display());
        };
        if !matches!(items.first(), Some(Node::Atom(head)) if head == "fp_lib_table") {
            bail!("Invalid footprint table {}", table.display());
        }
        for entry in items.into_iter().skip(1) {
            if !matches!(&entry, Node::List(parts) if matches!(parts.first(), Some(Node::Atom(head)) if head == "lib"))
            {
                continue;
            }
            let nickname = field_value(&entry, "name")
                .with_context(|| format!("Library entry missing name in {}", table.display()))?
                .to_owned();
            effective.insert(nickname, (entry, table.to_path_buf()));
        }
    }
    let mut entries = Vec::new();
    for (nickname, (entry, table)) in effective {
        // Disabled/unsupported local registrations still shadow global entries.
        if matches!(&entry, Node::List(parts) if parts.iter().any(|part| matches!(part, Node::List(fields) if matches!(fields.first(), Some(Node::Atom(head)) if head == "disabled"))))
        {
            continue;
        }
        if field_value(&entry, "type") != Some("KiCad") {
            continue;
        }
        let uri = field_value(&entry, "uri")
            .with_context(|| format!("Library {nickname} missing URI in {}", table.display()))?;
        let path = PathBuf::from(
            resolve_variables(uri, project, variables)
                .with_context(|| format!("Library {nickname} in {}", table.display()))?,
        );
        let path = if path.is_absolute() {
            path
        } else {
            table.parent().unwrap_or(Path::new(".")).join(path)
        };
        if !path.is_dir() {
            bail!(
                "Registered footprint library {nickname} is missing or not a directory: {}",
                path.display()
            );
        }
        std::fs::read_dir(&path).with_context(|| {
            format!(
                "Unreadable footprint library {nickname}: {}",
                path.display()
            )
        })?;
        entries.push(LibraryEntry { nickname, path });
    }
    Ok(entries)
}

#[cfg(test)]
pub fn footprint_libraries(
    table: &Path,
    project: Option<&Path>,
    variables: &BTreeMap<String, String>,
) -> Result<Vec<LibraryEntry>> {
    effective_footprint_libraries(table, None, project, variables)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_tables_and_avoids_nickname_collision() {
        let initial = r#"(fp_lib_table (version 7) (lib (name "UniLoader") (type "KiCad") (uri "/other.pretty") (options "keep")) (custom "keep too"))"#;
        let (text, name) =
            register_table(Some(initial), "fp_lib_table", "UniLoader", "/new.pretty").unwrap();
        assert_eq!(name, "UniLoader_2");
        assert!(text.contains("keep too"));
        assert!(text.contains("/other.pretty"));
        let (_, again) =
            register_table(Some(&text), "fp_lib_table", "UniLoader", "/new.pretty").unwrap();
        assert_eq!(again, name);
    }
    #[test]
    fn local_library_shadows_broken_global_before_path_resolution() {
        let root = std::env::temp_dir().join(format!(
            "uniloader-tables-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("project/libraries/Shared.pretty")).unwrap();
        let global = root.join("global-table");
        let local = root.join("project/fp-lib-table");
        std::fs::write(&global, r#"(fp_lib_table (lib (name "Shared") (type "KiCad") (uri "${UNILOADER_MISSING_TEST_VARIABLE}/Shared.pretty")))"#).unwrap();
        std::fs::write(&local, r#"(fp_lib_table (lib (name "Shared") (type "KiCad") (uri "${KIPRJMOD}/libraries/Shared.pretty")))"#).unwrap();
        let entries = effective_footprint_libraries(
            &global,
            Some(&local),
            Some(&root.join("project")),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].nickname, "Shared");
        assert_eq!(
            entries[0].path,
            root.join("project/libraries/Shared.pretty")
        );
        // An effective unresolved entry must still be reported.
        assert!(footprint_libraries(&global, None, &BTreeMap::new()).is_err());
        // A project-disabled entry shadows, rather than reviving the global one.
        std::fs::write(&local, r#"(fp_lib_table (lib (name "Shared") (type "KiCad") (uri "/not/present") (disabled)))"#).unwrap();
        assert!(
            effective_footprint_libraries(
                &global,
                Some(&local),
                Some(&root.join("project")),
                &BTreeMap::new()
            )
            .unwrap()
            .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn variable_expansion_reports_missing_and_recursion() {
        let vars = BTreeMap::from([("CUSTOM".into(), "${KIPRJMOD}/models".into())]);
        assert_eq!(
            resolve_variables("${CUSTOM}/x.step", Some(Path::new("/project")), &vars).unwrap(),
            "/project/models/x.step"
        );
        assert!(resolve_variables("${UNILOADER_MISSING_TEST_VARIABLE}", None, &vars).is_err());
        assert!(resolve_variables("${KIPRJMOD}", None, &vars).is_err());
    }
}
