use std::{
    env, fs,
    path::{self, Path, PathBuf},
    sync::mpsc,
};

use notify::{Event, EventKind, RecursiveMode, Watcher, event::CreateKind};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Deserialize, Serialize)]
struct Config {
    libraries: Option<LibConfig>,
}

impl Config {
    fn load_or_create() -> Result<Self, ConfigError> {
        let config_path =
            PathBuf::from(env::var("HOME").unwrap()).join(".config/uniloader/config.toml");

        if config_path.exists() {
            let contents = fs::read_to_string(&config_path).map_err(|_| ConfigError::ConfigErr)?;
            let config: Config = toml::from_str(&contents).map_err(|_| ConfigError::ConfigErr)?;
            Ok(config)
        } else {
            fs::create_dir_all(config_path.parent().unwrap())
                .map_err(|_| ConfigError::ConfigErr)?;
            let config = Config { libraries: None };
            let contents = toml::to_string_pretty(&config).map_err(|_| ConfigError::ConfigErr)?;
            fs::write(&config_path, contents).map_err(|_| ConfigError::ConfigErr)?;
            Ok(config)
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct LibConfig {
    symacsys_symbols: Option<PathBuf>,
    symacsys_footprints: Option<PathBuf>,
}

enum SourceType {
    SymacSys,
    UltraLibrarian,
}

#[derive(Error, Debug)]
enum ConfigError {
    #[error("Config error")]
    ConfigErr,
}

#[derive(Error, Debug)]
enum SourceError {
    #[error("Unknown Source")]
    SourceUnknown,
    #[error("Filename Error")]
    FilenameError,
}

fn main() -> notify::Result<()> {
    let config = Config::load_or_create().unwrap();
    if config.libraries.is_some() {
        if config.libraries.clone().unwrap().symacsys_symbols.is_some() {
            println!("{:?}", config.libraries.unwrap().symacsys_symbols.unwrap());
        }
    }
    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = notify::recommended_watcher(tx)?;
    let downloads = PathBuf::from(env::var("HOME").unwrap()).join("Downloads");
    watcher.watch(&downloads, RecursiveMode::NonRecursive)?;

    for res in rx {
        match res {
            Ok(event) => {
                if event.kind != EventKind::Create(CreateKind::File) {
                    continue;
                }
                if !validate_zip(&event.paths[0]) {
                    continue;
                }

                let Ok(source_type) = find_source_type(&event.paths[0]) else {
                    continue;
                };

                match source_type {
                    SourceType::SymacSys => println!("SymacSys detected"),
                    SourceType::UltraLibrarian => println!("UltraLibrarian detected"),
                }
            }
            Err(e) => println!("watch error: {:?}", e),
        }
    }

    Ok(())
}

fn validate_zip(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("zip"))
            .unwrap_or(false)
}

fn find_source_type(path: &Path) -> Result<SourceType, SourceError> {
    let name = path
        .file_name()
        .ok_or(SourceError::FilenameError)?
        .to_str()
        .ok_or(SourceError::FilenameError)?;

    if name.starts_with("LIB_") {
        Ok(SourceType::SymacSys)
    } else if name.starts_with("ul_") {
        Ok(SourceType::UltraLibrarian)
    } else {
        Err(SourceError::SourceUnknown)
    }
}
