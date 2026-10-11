use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub libraries: Libraries,
    pub options: Options,
    pub watch: Watch,
    pub destination: DestinationConfig,
    pub matching: Matching,
    pub ui: Ui,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Libraries {
    pub path: PathBuf,
    pub kicad_config: Option<PathBuf>,
    pub search_paths: Vec<PathBuf>,
    pub model_search_paths: Vec<PathBuf>,
    pub variables: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Options {
    pub symbols: bool,
    pub footprints: bool,
    pub models: bool,
    pub preserve_source: bool,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Watch {
    pub directories: Vec<PathBuf>,
    pub settle_seconds: u64,
    pub retry_seconds: u64,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DestinationConfig {
    pub policy: String,
    pub project: Option<PathBuf>,
    pub portable: bool,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Matching {
    pub enabled: bool,
    pub exact: String,
    pub ambiguous: String,
    pub tolerance: f64,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Ui {
    pub enabled: bool,
}

pub fn home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
pub fn xdg(name: &str, fallback: &str) -> PathBuf {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
}
pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("uniloader")
}
pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join("uniloader")
}
pub fn expand(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if s == "~" {
        home()
    } else if let Some(rest) = s.strip_prefix("~/") {
        home().join(rest)
    } else if p.is_absolute() {
        p.to_owned()
    } else {
        env::current_dir().unwrap_or_else(|_| home()).join(p)
    }
}
impl Default for Libraries {
    fn default() -> Self {
        Self {
            path: home().join("KiCad/libraries"),
            kicad_config: None,
            search_paths: vec![],
            model_search_paths: vec![],
            variables: BTreeMap::new(),
        }
    }
}
impl Default for Options {
    fn default() -> Self {
        Self {
            symbols: true,
            footprints: true,
            models: true,
            preserve_source: true,
        }
    }
}
impl Default for Watch {
    fn default() -> Self {
        Self {
            directories: vec![home().join("Downloads")],
            settle_seconds: 2,
            retry_seconds: 60,
        }
    }
}
impl Default for DestinationConfig {
    fn default() -> Self {
        Self {
            policy: "ask".into(),
            project: None,
            portable: true,
        }
    }
}
impl Default for Matching {
    fn default() -> Self {
        Self {
            enabled: true,
            exact: "reuse".into(),
            ambiguous: "ask".into(),
            tolerance: 0.15,
        }
    }
}
impl Default for Ui {
    fn default() -> Self {
        Self { enabled: true }
    }
}
impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = path
            .map(expand)
            .unwrap_or_else(|| xdg("XDG_CONFIG_HOME", ".config").join("uniloader/config.toml"));
        let mut cfg: Self = if path.exists() {
            toml::from_str(&fs::read_to_string(&path)?)
                .with_context(|| format!("invalid config {}", path.display()))?
        } else {
            if path.parent().is_some() {
                fs::create_dir_all(path.parent().unwrap())?;
            }
            let cfg = Self::default();
            fs::write(&path, toml::to_string_pretty(&cfg)?)?;
            cfg
        };
        cfg.libraries.path = expand(&cfg.libraries.path);
        cfg.libraries.kicad_config = cfg.libraries.kicad_config.as_deref().map(expand);
        cfg.destination.project = cfg.destination.project.as_deref().map(expand);
        for p in cfg
            .watch
            .directories
            .iter_mut()
            .chain(cfg.libraries.search_paths.iter_mut())
            .chain(cfg.libraries.model_search_paths.iter_mut())
        {
            *p = expand(p);
        }
        if !["ask", "global", "project"].contains(&cfg.destination.policy.as_str()) {
            bail!("destination.policy must be ask, global or project");
        }
        if !["reuse", "ask"].contains(&cfg.matching.exact.as_str()) {
            bail!("matching.exact must be reuse or ask");
        }
        if !["ask", "separate", "skip"].contains(&cfg.matching.ambiguous.as_str()) {
            bail!("matching.ambiguous must be ask, separate or skip");
        }
        if !cfg.matching.tolerance.is_finite() || cfg.matching.tolerance < 0.0 {
            bail!("matching.tolerance must be finite and nonnegative");
        }
        if cfg.watch.retry_seconds < cfg.watch.settle_seconds {
            bail!("watch.retry_seconds must be >= settle_seconds");
        }
        Ok(cfg)
    }
}
