//! Best-effort Linux project suggestions and optional desktop dialogs.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug, Deserialize)]
pub struct DestinationChoice {
    #[serde(default)]
    pub skip: bool,
    pub project: Option<PathBuf>,
    pub remember: bool,
}

#[derive(Debug, Serialize)]
pub struct Candidate {
    pub label: String,
    pub path: PathBuf,
    pub raw: String,
    pub differences: Vec<String>,
}

#[derive(Debug)]
pub enum FootprintChoice {
    Reuse(usize),
    Separate,
    Skip,
}

fn project_path(path: &Path) -> Option<PathBuf> {
    let candidate = match path.extension().and_then(|s| s.to_str()) {
        Some("kicad_pro") => path.to_path_buf(),
        Some("kicad_pcb" | "kicad_sch" | "pro" | "sch") => path.with_extension("kicad_pro"),
        _ => return None,
    };
    candidate
        .is_file()
        .then(|| fs::canonicalize(&candidate).unwrap_or(candidate))
}

/// Process arguments/cwd are suggestions, not an authoritative active-window API.
pub fn projects() -> Vec<PathBuf> {
    let mut found = BTreeSet::new();
    let Ok(processes) = fs::read_dir("/proc") else {
        return vec![];
    };
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    #[cfg(unix)]
    let owner = fs::metadata("/proc/self").ok().map(|m| m.uid());
    for process in processes.flatten() {
        #[cfg(unix)]
        if owner.is_some() && process.metadata().ok().map(|m| m.uid()) != owner {
            continue;
        }
        if !process
            .file_name()
            .to_string_lossy()
            .chars()
            .all(|c| c.is_ascii_digit())
        {
            continue;
        }
        let proc_path = process.path();
        let Ok(cmdline) = fs::read(proc_path.join("cmdline")) else {
            continue;
        };
        let args: Vec<_> = cmdline
            .split(|b| *b == 0)
            .filter(|b| !b.is_empty())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        let Some(executable) = args.first() else {
            continue;
        };
        let name = Path::new(executable)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        if !matches!(name.as_ref(), "kicad" | "pcbnew" | "eeschema") {
            continue;
        }
        let cwd = fs::read_link(proc_path.join("cwd")).ok();
        for arg in args.iter().skip(1).filter(|a| !a.starts_with('-')) {
            let path = Path::new(arg);
            let full = if path.is_absolute() {
                path.to_path_buf()
            } else if let Some(cwd) = &cwd {
                cwd.join(path)
            } else {
                continue;
            };
            if let Some(project) = project_path(&full) {
                found.insert(project);
            }
        }
        // The editor may omit the document argument. A project in its cwd is a
        // useful fallback, but it remains explicitly labelled a suggestion.
        if let Some(cwd) = &cwd
            && let Ok(entries) = fs::read_dir(cwd)
        {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|s| s.to_str()) == Some("kicad_pro")
                    && let Some(project) = project_path(&entry.path())
                {
                    found.insert(project);
                }
            }
        }
    }
    found.into_iter().collect()
}

fn dialog(request: Value) -> Result<Option<Value>> {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("UniLoader: no graphical session; choice remains pending");
        return Ok(None);
    }
    let spawn = Command::new(std::env::current_exe().context("locating UniLoader executable")?)
        .arg("__dialog")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn();
    let mut child = match spawn {
        Ok(child) => child,
        Err(error) => {
            eprintln!("UniLoader: could not start native dialog ({error}); choice remains pending");
            return Ok(None);
        }
    };
    let payload = serde_json::to_vec(&request)?;
    let write_result = child
        .stdin
        .take()
        .context("dialog stdin unavailable")
        .and_then(|mut input| {
            input
                .write_all(&payload)
                .context("writing native dialog request")
        });
    if let Err(error) = write_result {
        eprintln!("UniLoader: native dialog unavailable ({error:#}); choice remains pending");
        let _ = child.kill();
        let _ = child.wait();
        return Ok(None);
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(error) => {
            eprintln!("UniLoader: native dialog unavailable ({error}); choice remains pending");
            return Ok(None);
        }
    };
    if !output.status.success() {
        eprintln!("UniLoader: native dialog failed; choice remains pending");
        return Ok(None);
    }
    let value: Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(error) => {
            eprintln!(
                "UniLoader: invalid native dialog response ({error}); choice remains pending"
            );
            return Ok(None);
        }
    };
    Ok((!value.is_null()).then_some(value))
}

/// Internal entry point: run the native window on the subprocess main thread.
/// It deliberately avoids loading configuration or acquiring importer locks.
pub fn run_internal_dialog() -> Result<()> {
    const LIMIT: u64 = 32 * 1024 * 1024;
    let mut bytes = Vec::new();
    std::io::stdin().take(LIMIT + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= LIMIT,
        "native dialog request exceeds 32 MiB"
    );
    let request: Value = serde_json::from_slice(&bytes).context("invalid native dialog request")?;
    let response =
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            eprintln!("UniLoader: no graphical session; choice remains pending");
            None
        } else {
            match crate::gui::run(request) {
                Ok(value) => value,
                Err(error) => {
                    eprintln!(
                        "UniLoader: native dialog unavailable ({error:#}); choice remains pending"
                    );
                    None
                }
            }
        };
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, &response)?;
    writeln!(output)?;
    Ok(())
}

pub fn destination(projects: &[PathBuf], component: &str) -> Result<Option<DestinationChoice>> {
    dialog(json!({"mode": "destination", "projects": projects, "component": component}))?
        .map(serde_json::from_value)
        .transpose()
        .context("invalid destination choice")
}

pub fn compare(incoming: &str, candidates: &[Candidate]) -> Result<Option<FootprintChoice>> {
    if candidates.is_empty() {
        return Ok(None);
    }
    let Some(value) =
        dialog(json!({"mode": "compare", "incoming": incoming, "candidates": candidates}))?
    else {
        return Ok(None);
    };
    match value["action"].as_str() {
        Some("reuse") => {
            let index = value["index"]
                .as_u64()
                .context("missing footprint candidate index")? as usize;
            anyhow::ensure!(
                index < candidates.len(),
                "footprint candidate index out of range"
            );
            Ok(Some(FootprintChoice::Reuse(index)))
        }
        Some("separate") => Ok(Some(FootprintChoice::Separate)),
        Some("skip") => Ok(Some(FootprintChoice::Skip)),
        _ => anyhow::bail!("invalid footprint choice"),
    }
}

pub fn choose_file(title: &str, paths: &[PathBuf]) -> Result<Option<PathBuf>> {
    let Some(value) = dialog(json!({"mode": "file", "title": title, "paths": paths}))? else {
        return Ok(None);
    };
    let path: PathBuf =
        serde_json::from_value(value["path"].clone()).context("invalid file choice")?;
    anyhow::ensure!(paths.contains(&path), "selected file was not a candidate");
    Ok(Some(path))
}

/// Optional desktop notification; logs remain the authoritative import report.
pub fn notify(message: &str) {
    let _ = Command::new("notify-send")
        .args(["--app-name=UniLoader", "UniLoader", message])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_existing_project_from_editor_document() {
        let dir = std::env::temp_dir().join(format!("uniloader-desktop-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let project = dir.join("board.kicad_pro");
        fs::write(&project, "{}").unwrap();
        assert_eq!(
            project_path(&dir.join("board.kicad_pcb")),
            Some(fs::canonicalize(&project).unwrap())
        );
        assert_eq!(project_path(&dir.join("missing.kicad_sch")), None);
        fs::remove_dir_all(dir).unwrap();
    }
}
