use std::{
    env,
    path::{Path, PathBuf},
    sync::mpsc,
};

use notify::{Event, RecursiveMode, Result, Watcher};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Config {
    libraries: LibConfig,
}

impl Config {}

#[derive(Debug, Deserialize)]
struct LibConfig {
    component_search_engine_symbols: PathBuf,
    component_serach_engine_footprints: PathBuf,
}

fn main() -> Result<()> {
    let (tx, rx) = mpsc::channel::<Result<Event>>();
    let mut watcher = notify::recommended_watcher(tx)?;
    let downloads = PathBuf::from(env::var("HOME").unwrap()).join("Downloads");
    watcher.watch(&downloads, RecursiveMode::Recursive)?;

    for res in rx {
        match res {
            Ok(event) => println!("event: {:?}", event.paths[0]),
            Err(e) => println!("watch error: {:?}", e),
        }
    }

    Ok(())
}

fn validate_lib(path: PathBuf) -> bool {
    if path.is_file() {
        let filename = path.file_name();
    }
    return false;
}
