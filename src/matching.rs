//! Conservative exact matching and deliberately simple candidate detection.
//!
//! Exact normalization ignores only the root footprint name and generated
//! uuid/tstamp records in recognized footprint elements. All other fields
//! (including unknown subtrees) remain.
use crate::sexpr::Node;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn contextual_model(path: &str, source: &Path) -> String {
    let normalized = path.replace('\\', "/");
    let mut expanded = normalized.clone();
    // Expand known environment variables, leaving unsupported ones explicit.
    while let Some(start) = expanded.find("${") {
        let Some(end) = expanded[start + 2..].find('}') else {
            break;
        };
        let end = start + 2 + end;
        let key = &expanded[start + 2..end];
        let value = if key == "KIPRJMOD" {
            source
                .parent()
                .into_iter()
                .flat_map(Path::ancestors)
                .find(|p| {
                    fs::read_dir(p).ok().is_some_and(|entries| {
                        entries
                            .filter_map(Result::ok)
                            .any(|e| e.path().extension().is_some_and(|x| x == "kicad_pro"))
                    })
                })
                .map(|p| p.to_string_lossy().into_owned())
                .or_else(|| std::env::var(key).ok())
        } else {
            std::env::var(key).ok()
        };
        let Some(value) = value else {
            break;
        };
        expanded.replace_range(start..=end, &value);
        // Avoid pathological self-referential variables.
        if value.contains("${") {
            break;
        }
    }
    let path = PathBuf::from(&expanded);
    let resolved = if path.is_absolute() {
        path
    } else {
        source.parent().unwrap_or(Path::new(".")).join(path)
    };
    if let Ok(bytes) = fs::read(&resolved) {
        return format!("model-content:{}", digest(&bytes));
    }
    // Relative model strings from different libraries are not proof of identity.
    // Include lexical origin even when canonicalization cannot resolve the model.
    let origin = fs::canonicalize(source).unwrap_or_else(|_| {
        if source.is_absolute() {
            source.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(source)
        }
    });
    format!(
        "unresolved-model:{}:{}",
        origin.to_string_lossy(),
        normalized
    )
}

pub fn exact_fingerprint(node: &Node, source_path: &Path) -> String {
    fn normalized(n: &Node, source: &Path, root: bool) -> Node {
        match n {
            Node::List(v) => {
                // Do not normalize an unknown subtree: its identifiers may have
                // semantics we do not understand.
                if !matches!(
                    n.head(),
                    Some(
                        "footprint"
                            | "module"
                            | "pad"
                            | "fp_line"
                            | "fp_arc"
                            | "fp_rect"
                            | "fp_circle"
                            | "fp_poly"
                            | "fp_curve"
                            | "fp_text"
                            | "fp_text_box"
                            | "property"
                            | "model"
                    )
                ) {
                    return n.clone();
                }
                let mut children: Vec<Node> = v
                    .iter()
                    .filter(|c| !matches!(c.head(), Some("uuid" | "tstamp")))
                    .map(|c| normalized(c, source, false))
                    .collect();
                if root && matches!(n.head(), Some("footprint" | "module")) && children.len() > 1 {
                    children[1] = Node::string("<footprint-name>");
                }
                if n.head() == Some("model")
                    && let Some(path) = n.value()
                {
                    children[1] = Node::string(contextual_model(path, source));
                }
                Node::List(children)
            }
            _ => n.clone(),
        }
    }
    digest(normalized(node, source_path, true).to_string().as_bytes())
}

fn pads(n: &Node) -> BTreeMap<String, Vec<&Node>> {
    let mut result: BTreeMap<String, Vec<&Node>> = BTreeMap::new();
    for pad in n.children().iter().filter(|n| n.head() == Some("pad")) {
        result
            .entry(pad.value().unwrap_or("").to_owned())
            .or_default()
            .push(pad);
    }
    result
}
fn coordinates(n: &Node, field: &str) -> Option<(f64, f64)> {
    let v = n.field(field)?.children();
    let x = v.get(1)?.atom()?.parse::<f64>().ok()?;
    let y = v.get(2)?.atom()?.parse::<f64>().ok()?;
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

pub fn similar(a: &Node, b: &Node, tolerance: f64) -> bool {
    if let (Some(an), Some(bn)) = (a.value(), b.value())
        && !an.is_empty()
        && an == bn
    {
        return true;
    }
    let ap = pads(a);
    let bp = pads(b);
    if ap.is_empty() || ap.len() != bp.len() {
        return false;
    }
    let tolerance = if tolerance.is_finite() {
        tolerance.max(0.0)
    } else {
        0.0
    };
    for (number, aa) in ap {
        let Some(bb) = bp.get(&number) else {
            return false;
        };
        if aa.len() != bb.len() {
            return false;
        }
        // Multiple pads with the same number are common. Match one-to-one,
        // without depending on file ordering.
        let mut used = vec![false; bb.len()];
        for pad in aa {
            let Some((ax, ay)) = coordinates(pad, "at") else {
                return false;
            };
            let Some(index) = bb.iter().enumerate().position(|(i, other)| {
                !used[i]
                    && coordinates(other, "at").is_some_and(|(bx, by)| {
                        (ax - bx).abs() <= tolerance && (ay - by).abs() <= tolerance
                    })
            }) else {
                return false;
            };
            used[index] = true;
        }
    }
    true
}

pub fn differences(a: &Node, b: &Node) -> Vec<String> {
    let mut result = Vec::new();
    let ap = pads(a);
    let bp = pads(b);
    let ac: usize = ap.values().map(Vec::len).sum();
    let bc: usize = bp.values().map(Vec::len).sum();
    if ac != bc {
        result.push(format!("Pad count: incoming {ac}, candidate {bc}"));
    }
    let ak: Vec<_> = ap.keys().collect();
    let bk: Vec<_> = bp.keys().collect();
    if ak != bk {
        result.push(format!("Pad numbers: incoming {ak:?}, candidate {bk:?}"));
    }
    for (number, aa) in &ap {
        if let Some(bb) = bp.get(number) {
            for (a, b) in aa.iter().zip(bb.iter()) {
                for (index, label) in [(2, "type"), (3, "shape")] {
                    let av = a
                        .children()
                        .get(index)
                        .and_then(Node::atom)
                        .unwrap_or("missing");
                    let bv = b
                        .children()
                        .get(index)
                        .and_then(Node::atom)
                        .unwrap_or("missing");
                    if av != bv {
                        result.push(format!("Pad {number} {label}: {av} → {bv}"));
                    }
                }
                for field in [
                    "at",
                    "size",
                    "drill",
                    "layers",
                    "solder_mask_margin",
                    "solder_paste_margin",
                    "solder_paste_margin_ratio",
                ] {
                    let av = a.field(field).map(ToString::to_string);
                    let bv = b.field(field).map(ToString::to_string);
                    if av != bv {
                        result.push(format!(
                            "Pad {number} {field}: {} → {}",
                            av.as_deref().unwrap_or("default"),
                            bv.as_deref().unwrap_or("default")
                        ));
                    }
                }
            }
        }
    }
    for field in [
        "attr",
        "layer",
        "solder_mask_margin",
        "solder_paste_margin",
        "solder_paste_ratio",
        "clearance",
    ] {
        if a.field(field) != b.field(field) {
            result.push(format!("Footprint {field} differs"));
        }
    }
    let am: Vec<_> = a
        .children()
        .iter()
        .filter(|n| n.head() == Some("model"))
        .collect();
    let bm: Vec<_> = b
        .children()
        .iter()
        .filter(|n| n.head() == Some("model"))
        .collect();
    if am != bm {
        result.push("3D model references or transforms differ".into());
    }
    let graphics = |n: &Node| -> Vec<String> {
        n.children()
            .iter()
            .filter(|c| {
                c.head()
                    .is_some_and(|h| h.starts_with("fp_") || h == "property")
            })
            .map(ToString::to_string)
            .collect()
    };
    if graphics(a) != graphics(b) {
        result.push("Graphics, labels or courtyard differ".into());
    }
    if result.is_empty() && a != b {
        result.push("Other fields or generated identifiers differ; inspect the files".into());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sexpr::parse;
    #[test]
    fn exact_ignores_only_name_whitespace_and_identifiers() {
        let a = parse(
            r#"(footprint "A" (uuid "a") (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu")))"#,
        )
        .unwrap();
        let b = parse(
            r#"(footprint "B" (uuid "b") (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu")))"#,
        )
        .unwrap();
        assert_eq!(
            exact_fingerprint(&a, Path::new("a")),
            exact_fingerprint(&b, Path::new("b"))
        );
        let c = parse(r#"(footprint "A" (uuid "a") (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu")) (unknown_future 1))"#).unwrap();
        assert_ne!(
            exact_fingerprint(&a, Path::new("a")),
            exact_fingerprint(&c, Path::new("a"))
        );
        let future_a = parse(r#"(footprint "A" (future (uuid "a")))"#).unwrap();
        let future_b = parse(r#"(footprint "A" (future (uuid "b")))"#).unwrap();
        assert_ne!(
            exact_fingerprint(&future_a, Path::new("a")),
            exact_fingerprint(&future_b, Path::new("a"))
        );
    }
    #[test]
    fn relative_models_are_contextual_and_content_based() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let fp = parse(r#"(footprint "A" (model "part.step" (offset (xyz 0 0 0))))"#).unwrap();
        let pa = a.join("a.kicad_mod");
        let pb = b.join("b.kicad_mod");
        assert_ne!(exact_fingerprint(&fp, &pa), exact_fingerprint(&fp, &pb));
        fs::write(a.join("part.step"), "same model").unwrap();
        fs::write(b.join("part.step"), "same model").unwrap();
        assert_eq!(exact_fingerprint(&fp, &pa), exact_fingerprint(&fp, &pb));
        fs::write(b.join("part.step"), "different model").unwrap();
        assert_ne!(exact_fingerprint(&fp, &pa), exact_fingerprint(&fp, &pb));
    }
    #[test]
    fn similar_is_only_a_candidate_and_keeps_pad_numbering() {
        let a = parse(r#"(footprint "A" (pad "1" smd rect (at 0 0) (size 1 1)))"#).unwrap();
        let b = parse(r#"(footprint "B" (pad "1" smd rect (at 0.01 0) (size 2 1)))"#).unwrap();
        let c = parse(r#"(footprint "C" (pad "2" smd rect (at 0 0) (size 1 1)))"#).unwrap();
        assert!(similar(&a, &b, 0.05));
        assert!(!similar(&a, &c, 0.05));
        assert!(differences(&a, &b).iter().any(|d| d.contains("size")));
        assert_ne!(
            exact_fingerprint(&a, Path::new("a")),
            exact_fingerprint(&b, Path::new("b"))
        );
    }
}
