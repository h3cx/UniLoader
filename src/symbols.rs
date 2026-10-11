//! Merge imported symbol libraries without silently replacing existing symbols.
use crate::sexpr::Node;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

fn name(node: &Node) -> Result<&str> {
    let name = node.value().context("symbol has no name")?;
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':', '\0']) {
        bail!("invalid KiCad symbol name: {name:?}");
    }
    Ok(name)
}
fn find<'a>(library: &'a Node, name: &str) -> Option<&'a Node> {
    library
        .children()
        .iter()
        .find(|n| n.head() == Some("symbol") && n.value() == Some(name))
}
fn rename(node: &mut Node, old: &str, new: &str) {
    if node.head() == Some("symbol")
        && let Some(current) = node.value()
    {
        let renamed = if current == old {
            Some(new.to_owned())
        } else {
            current
                .strip_prefix(&format!("{old}_"))
                .map(|suffix| format!("{new}_{suffix}"))
        };
        if let Some(renamed) = renamed {
            node.children_mut()[1] = Node::string(renamed);
        }
    }
    if let Node::List(children) = node {
        for child in children {
            rename(child, old, new);
        }
    }
}

fn merge_one(
    old: &str,
    source: &BTreeMap<String, Node>,
    resolved: &mut BTreeMap<String, String>,
    visiting: &mut BTreeSet<String>,
    current: &mut Node,
    messages: &mut Vec<String>,
) -> Result<String> {
    if let Some(final_name) = resolved.get(old) {
        return Ok(final_name.clone());
    }
    if !visiting.insert(old.to_owned()) {
        bail!("cyclic symbol inheritance involving {old}");
    }
    let mut symbol = source.get(old).context("incoming symbol missing")?.clone();
    if let Some(parent) = symbol.field("extends") {
        let parent = parent
            .value()
            .context("extends has no parent name")?
            .to_owned();
        let final_parent = if source.contains_key(&parent) {
            merge_one(&parent, source, resolved, visiting, current, messages)?
        } else if find(current, &parent).is_some() {
            parent
        } else {
            bail!("pending: symbol {old} extends missing parent {parent}");
        };
        symbol.field_mut("extends").unwrap().children_mut()[1] = Node::string(final_parent);
    }
    let final_name = match find(current, old) {
        None => {
            current.children_mut().push(symbol);
            old.to_owned()
        }
        Some(existing) if existing == &symbol => {
            messages.push(format!("Skip identical symbol {old}"));
            old.to_owned()
        }
        Some(_) => {
            // Fingerprint the original name with already-resolved inheritance.
            // Renaming this same content again therefore yields the same candidate.
            let hash = format!("{:x}", Sha256::digest(symbol.to_string().as_bytes()));
            let mut selected = None;
            for n in 0..1000 {
                let candidate = if n == 0 {
                    format!("{old}_{}", &hash[..8])
                } else {
                    format!("{old}_{}_{n}", &hash[..8])
                };
                let mut renamed = symbol.clone();
                rename(&mut renamed, old, &candidate);
                match find(current, &candidate) {
                    Some(existing) if existing == &renamed => {
                        messages.push(format!("Skip identical renamed symbol {candidate}"));
                        selected = Some(candidate);
                        break;
                    }
                    Some(_) => continue,
                    None => {
                        messages.push(format!("Rename conflicting symbol {old} to {candidate}"));
                        current.children_mut().push(renamed);
                        selected = Some(candidate);
                        break;
                    }
                }
            }
            selected.context("cannot allocate a conflict-free symbol name")?
        }
    };
    visiting.remove(old);
    resolved.insert(old.to_owned(), final_name.clone());
    Ok(final_name)
}

pub fn merge(mut current: Node, incoming: Vec<Node>) -> Result<(Node, Vec<String>)> {
    if current.head() != Some("kicad_symbol_lib") {
        bail!("target is not a symbol library");
    }
    let mut messages = Vec::new();
    let mut version = current
        .field("version")
        .and_then(Node::value)
        .context("target symbol library missing version")?
        .parse::<u64>()?;
    for library in incoming {
        if library.head() != Some("kicad_symbol_lib") {
            bail!("incoming asset is not a symbol library");
        }
        version = version.max(
            library
                .field("version")
                .and_then(Node::value)
                .context("incoming symbol library missing version")?
                .parse::<u64>()?,
        );
        for item in library.children().iter().skip(1).filter(|n| {
            !matches!(
                n.head(),
                Some("symbol" | "version" | "generator" | "generator_version")
            )
        }) {
            if current.children().contains(item) {
                continue;
            }
            if current.children().iter().any(|n| n.head() == item.head()) {
                bail!(
                    "pending: conflicting library-level metadata {:?}",
                    item.head()
                );
            }
            current.children_mut().push(item.clone());
        }
        let mut source = BTreeMap::new();
        let mut order = Vec::new();
        for symbol in library
            .children()
            .iter()
            .filter(|n| n.head() == Some("symbol"))
        {
            let old = name(symbol)?.to_owned();
            if let Some(previous) = source.insert(old.clone(), symbol.clone()) {
                if previous != *symbol {
                    bail!(
                        "pending: conflicting definitions of symbol {old} inside one incoming library"
                    );
                }
            } else {
                order.push(old);
            }
        }
        // This map belongs to one source library. Unrelated libraries can have
        // different parents with the same name without retargeting each other.
        let mut resolved = BTreeMap::new();
        let mut visiting = BTreeSet::new();
        for old in order {
            merge_one(
                &old,
                &source,
                &mut resolved,
                &mut visiting,
                &mut current,
                &mut messages,
            )?;
        }
    }
    current.field_mut("version").unwrap().children_mut()[1] = Node::symbol(version.to_string());
    Ok((current, messages))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sexpr::parse;
    fn library(symbols: &str) -> Node {
        parse(&format!(
            "(kicad_symbol_lib (version 20231120) (generator test) {symbols})"
        ))
        .unwrap()
    }
    fn count(lib: &Node) -> usize {
        lib.children()
            .iter()
            .filter(|n| n.head() == Some("symbol"))
            .count()
    }
    #[test]
    fn conflicts_rename_units_and_repeat_import_is_idempotent() {
        let base = library(r#"(symbol "Part" (property "Value" "original"))"#);
        let incoming = library(
            r#"(symbol "Part" (property "Value" "changed") (symbol "Part_0_1" (future "Ω (keep)")))"#,
        );
        let (merged, _) = merge(base, vec![incoming.clone()]).unwrap();
        assert_eq!(count(&merged), 2);
        let renamed = merged
            .children()
            .iter()
            .rfind(|n| n.head() == Some("symbol"))
            .unwrap();
        assert_eq!(
            renamed.field("symbol").unwrap().value().unwrap(),
            format!("{}_0_1", renamed.value().unwrap())
        );
        let (again, _) = merge(merged.clone(), vec![incoming]).unwrap();
        assert_eq!(merged, again);
    }
    #[test]
    fn inherited_conflicts_resolve_parent_first_and_reimport_is_idempotent() {
        let base = library(
            r#"(symbol "Parent" (property "Value" "original")) (symbol "Child" (extends "Parent"))"#,
        );
        let incoming = library(
            r#"(symbol "Child" (extends "Parent")) (symbol "Parent" (property "Value" "changed"))"#,
        );
        let (merged, _) = merge(base, vec![incoming.clone()]).unwrap();
        assert_eq!(count(&merged), 4);
        let child = merged
            .children()
            .iter()
            .find(|n| {
                n.head() == Some("symbol") && n.value().is_some_and(|v| v.starts_with("Child_"))
            })
            .unwrap();
        let parent = child.field("extends").unwrap().value().unwrap();
        assert!(parent.starts_with("Parent_"));
        assert!(find(&merged, parent).is_some());
        assert_eq!(merge(merged.clone(), vec![incoming]).unwrap().0, merged);
    }
    #[test]
    fn rename_maps_are_scoped_to_source_library() {
        let base = library(r#"(symbol "Parent" (property "Value" "original"))"#);
        let a = library(
            r#"(symbol "Parent" (property "Value" "changed")) (symbol "ChildA" (extends "Parent"))"#,
        );
        let b = library(
            r#"(symbol "Parent" (property "Value" "original")) (symbol "ChildB" (extends "Parent"))"#,
        );
        let (merged, _) = merge(base, vec![a, b]).unwrap();
        assert!(
            find(&merged, "ChildA")
                .unwrap()
                .field("extends")
                .unwrap()
                .value()
                .unwrap()
                .starts_with("Parent_")
        );
        assert_eq!(
            find(&merged, "ChildB")
                .unwrap()
                .field("extends")
                .unwrap()
                .value(),
            Some("Parent")
        );
    }
    #[test]
    fn rejects_missing_and_cyclic_inheritance() {
        let base = library("");
        assert!(
            merge(
                base.clone(),
                vec![library(r#"(symbol "A" (extends "Missing"))"#)]
            )
            .is_err()
        );
        assert!(
            merge(
                base,
                vec![library(
                    r#"(symbol "A" (extends "B")) (symbol "B" (extends "A"))"#
                )]
            )
            .is_err()
        );
    }
    #[test]
    fn preserves_unknown_metadata_and_max_version() {
        let base = library("");
        let future = parse(r#"(kicad_symbol_lib (version 20261101) (future_library "Ω") (symbol "A" (unknown (uuid "meaningful"))))"#).unwrap();
        let (merged, _) = merge(base, vec![future]).unwrap();
        assert_eq!(merged.field("version").unwrap().value(), Some("20261101"));
        assert_eq!(merged.field("future_library").unwrap().value(), Some("Ω"));
        assert!(find(&merged, "A").unwrap().field("unknown").is_some());
    }
}
