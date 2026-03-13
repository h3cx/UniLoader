use std::{
    env,
    fs::{self, File},
    ops::Not,
    path::{self, Path, PathBuf},
    sync::mpsc,
};

use chrono::Local;
use notify::{Event, EventKind, RecursiveMode, Watcher, event::CreateKind};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zip::ZipArchive;

#[derive(Debug, Deserialize, Serialize)]
struct Config {
    libraries: LibConfig,
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
            let config = Config {
                libraries: LibConfig {
                    path: PathBuf::from("~/KiCad/libraries/"),
                },
            };
            let contents = toml::to_string_pretty(&config).map_err(|_| ConfigError::ConfigErr)?;
            fs::write(&config_path, contents).map_err(|_| ConfigError::ConfigErr)?;
            Ok(config)
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct LibConfig {
    path: PathBuf,
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

#[derive(Error, Debug)]
enum ImportError {
    #[error("Unknown Path")]
    InvalidPath,
    #[error("Corrputed Zip")]
    InvalidZip,
}

struct SymbolLibrary {
    version: String,
    symbols: Vec<Symbol>,
}

struct Symbol {
    name: String,
    raw: String,
}

impl SymbolLibrary {
    fn new() -> Self {
        SymbolLibrary {
            version: "20211014".to_string(),
            symbols: Vec::new(),
        }
    }
    fn write(&self) -> String {
        let mut out = format!(
            "(kicad_symbol_lib\n\t(version {})\n\t(generator \"uniloader\")\n",
            self.version,
        );

        for sym in &self.symbols {
            out.push_str(&sym.raw);
            out.push_str("\n");
        }

        out.push_str(")");
        out
    }

    fn from_str(input: &str) -> Self {
        let input = input.replace("\r\n", "\n");
        let mut version = String::new();
        let mut symbols = Vec::new();

        if let Some(v_start) = input.find("(version ") {
            let rest = &input[v_start + 9..];
            if let Some(v_end) = rest.find(')') {
                version = rest[..v_end].trim().to_string();
            }
        }

        let mut i = 0;
        let chars: Vec<char> = input.chars().collect();

        while i < chars.len() {
            if input[i..].starts_with("(symbol ") {
                let start = i;
                let mut depth = 0;

                while i < chars.len() {
                    if chars[i] == '(' {
                        depth += 1;
                    } else if chars[i] == ')' {
                        depth -= 1;

                        if depth == 0 {
                            let end = i + 1;
                            let raw = input[start..end].to_string();

                            let name = raw.split('"').nth(1).unwrap_or("unknown").to_string();

                            symbols.push(Symbol { name, raw });

                            break;
                        }
                    }
                    i += 1;
                }
            }
            i += 1;
        }

        Self { version, symbols }
    }
}

fn main() -> notify::Result<()> {
    let config = Config::load_or_create().unwrap();
    println!("{:?}", config.libraries.path);
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
                    SourceType::SymacSys => {
                        println!("SymacSys detected, Installing...");
                        let Ok(_) = import_symacsys(&event.paths[0], &config) else {
                            continue;
                        };
                    }
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

fn extract_zip(path: &Path, config: &Config) -> Result<PathBuf, ImportError> {
    let file = File::open(path).map_err(|_| ImportError::InvalidPath)?;
    let mut archive = ZipArchive::new(file).map_err(|_| ImportError::InvalidZip)?;

    let Some(file_name) = path.file_stem() else {
        return Err(ImportError::InvalidPath);
    };

    let tmp_path = config.libraries.path.join("tmp").join(file_name);

    fs::create_dir_all(&tmp_path).map_err(|_| ImportError::InvalidPath)?;

    archive
        .extract(&tmp_path)
        .map_err(|_| ImportError::InvalidPath)?;

    fs::remove_file(path).map_err(|_| ImportError::InvalidPath)?;
    Ok(tmp_path)
}

fn import_symacsys(path: &Path, config: &Config) -> Result<(), ImportError> {
    let tmp_path = extract_zip(path, config)?;

    let dirs = fs::read_dir(&tmp_path).map_err(|_| ImportError::InvalidPath)?;

    let Some(data_dir) = dirs
        .filter_map(|e| e.ok())
        .find(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|entry| entry.path())
    else {
        return Err(ImportError::InvalidPath);
    };

    let kicad_dir = data_dir.join("KiCad");

    let results = fs::read_dir(&kicad_dir).map_err(|_| ImportError::InvalidPath)?;

    let Some(sym) = results
        .filter_map(|e| e.ok())
        .find(|entry| {
            entry
                .path()
                .extension()
                .map(|ext| ext == "kicad_sym")
                .unwrap_or(false)
        })
        .map(|entry| entry.path())
    else {
        return Err(ImportError::InvalidPath);
    };

    let sym_str = fs::read_to_string(&sym).map_err(|_| ImportError::InvalidPath)?;

    let new_lib = SymbolLibrary::from_str(&sym_str);

    let symacsys_path = config.libraries.path.join("SymacSys");
    let symacsys_sym = symacsys_path.join("SymacSys_Components.kicad_sym");

    fs::create_dir_all(symacsys_path).map_err(|_| ImportError::InvalidPath)?;

    let mut current_lib: SymbolLibrary;

    if symacsys_sym.exists() {
        let symacsys_sym_str =
            fs::read_to_string(&symacsys_sym).map_err(|_| ImportError::InvalidPath)?;
        current_lib = SymbolLibrary::from_str(&symacsys_sym_str);
    } else {
        current_lib = SymbolLibrary::new();
        let symacsys_sym_str = current_lib.write();
        fs::write(&symacsys_sym, symacsys_sym_str).map_err(|_| ImportError::InvalidPath)?;
    }

    for symbol in new_lib.symbols {
        current_lib.symbols.push(symbol);
    }

    let out_lib = current_lib.write();
    fs::write(&symacsys_sym, out_lib).map_err(|_| ImportError::InvalidPath)?;

    println!("{:?}", tmp_path);

    fs::remove_dir_all(tmp_path).map_err(|_| {
        println!("Error rmdir");
        ImportError::InvalidPath
    })?;

    Ok(())
}
