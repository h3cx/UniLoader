use crate::sexpr::{self, Node};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

pub struct Bundle {
    pub root: PathBuf,
    pub symbols: Vec<(PathBuf, Node)>,
    pub footprints: Vec<(PathBuf, Node)>,
    pub provider: String,
    _temp: Option<tempfile::TempDir>,
}
pub fn load(path: &Path) -> Result<Bundle> {
    let mut temp = None;
    let root = if path.is_dir() {
        fs::canonicalize(path)?
    } else {
        let mut archive =
            zip::ZipArchive::new(fs::File::open(path)?).context("not a complete readable ZIP")?;
        fs::create_dir_all(crate::config::cache_dir())?;
        let dir = tempfile::Builder::new()
            .prefix("extract-")
            .tempdir_in(crate::config::cache_dir())?;
        let mut total = 0u64;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            let enclosed = entry.enclosed_name().context("unsafe archive path")?;
            if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
                bail!("archive contains symbolic link");
            }
            total = total
                .checked_add(entry.size())
                .context("archive too large")?;
            if total > 2 * 1024 * 1024 * 1024 {
                bail!("archive exceeds 2 GiB expanded limit");
            }
            let out = dir.path().join(enclosed);
            if entry.is_dir() {
                fs::create_dir_all(out)?;
            } else {
                fs::create_dir_all(out.parent().unwrap())?;
                let mut f = fs::File::create(out)?;
                std::io::copy(&mut entry, &mut f)?;
            }
        }
        let root = dir.path().to_owned();
        temp = Some(dir);
        root
    };
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let mut provider = if name.starts_with("lib_") {
        "SamacSys"
    } else if name.starts_with("ul_") {
        "UltraLibrarian"
    } else {
        "Generic"
    }
    .to_owned();
    let mut symbols = vec![];
    let mut footprints = vec![];
    for entry in WalkDir::new(&root).follow_links(false).sort_by_file_name() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let p = entry.path();
        let ext = p
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if ext == "kicad_sym" || ext == "kicad_mod" {
            let raw = fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
            let node = sexpr::parse(&raw).with_context(|| format!("parsing {}", p.display()))?;
            if ext == "kicad_sym" {
                if node.head() != Some("kicad_symbol_lib") {
                    bail!("{} is not a symbol library", p.display());
                }
                symbols.push((p.to_owned(), node));
            } else {
                if !matches!(node.head(), Some("footprint" | "module")) {
                    bail!("{} is not a footprint", p.display());
                }
                footprints.push((p.to_owned(), node));
            }
        } else if entry
            .file_name()
            .to_string_lossy()
            .to_lowercase()
            .contains("how-to-import")
        {
            let mut s = String::new();
            fs::File::open(p)?
                .take(1024 * 1024)
                .read_to_string(&mut s)
                .ok();
            if s.to_lowercase().contains("snapeda") {
                provider = "SnapMagic".into();
            }
        }
    }
    if symbols.is_empty() && footprints.is_empty() {
        bail!("no KiCad symbols or footprints found");
    }
    Ok(Bundle {
        root,
        symbols,
        footprints,
        provider,
        _temp: temp,
    })
}
