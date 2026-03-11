use std::{env, path::PathBuf};

use inotify::{Inotify, WatchMask};
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

fn main() -> std::io::Result<()> {
    let downloads = PathBuf::from(env::var("HOME").unwrap()).join("Downloads");

    println!("Watching {:?}", downloads);

    let mut inotify = Inotify::init()?;

    inotify.watches().add(&downloads, WatchMask::MOVED_TO)?;

    let mut buffer = [0u8; 4096];

    loop {
        let events = inotify.read_events_blocking(&mut buffer)?;

        for event in events {
            if let Some(name) = event.name {
                if let Some(name_str) = name.to_str() {
                    if name_str.starts_with("LIB") {
                        println!("Component Search Engine Detected");
                    }
                }
            }
        }
    }
}
