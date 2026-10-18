//! Actual filesystem watcher subprocess: no display, user state or KiCad needed.
use std::{
    fs,
    io::{Cursor, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};
use zip::{ZipWriter, write::SimpleFileOptions};

struct Watcher(Child);
impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn until(label: &str, log: &Path, mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if check() {
            return;
        }
        thread::sleep(Duration::from_millis(40));
    }
    panic!(
        "timed out waiting for {label}; watcher log:\n{}",
        fs::read_to_string(log).unwrap_or_default()
    );
}

fn archive(name: &str, pad: usize) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let fp = format!(
        r#"(footprint "{name}" (version 20240108) (layer "F.Cu") (pad "{pad}" smd rect (at 0 0) (size 1 1) (layers "F.Cu" "F.Paste" "F.Mask")))"#
    );
    let sym = format!(
        r#"(kicad_symbol_lib (version 20231120) (generator "watch-fixture") (symbol "{name}" (property "Reference" "U") (property "Footprint" "{name}")))"#
    );
    for (file, data) in [
        (format!("{name}.kicad_mod"), fp),
        (format!("{name}.kicad_sym"), sym),
    ] {
        zip.start_file(file, SimpleFileOptions::default()).unwrap();
        zip.write_all(data.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn completed(root: &Path) -> usize {
    let Ok(entries) = fs::read_dir(root.join("data/uniloader/imports")) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            fs::read(entry.path())
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .is_some_and(|record| record["status"] == "completed")
        })
        .count()
}

fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>, SystemTime)> {
    let mut files = vec![];
    for tree in [root.join("project"), root.join("data/uniloader/imports")] {
        for entry in walkdir::WalkDir::new(tree).into_iter().flatten() {
            if entry.file_type().is_file() {
                files.push((
                    entry.path().to_owned(),
                    fs::read(entry.path()).unwrap(),
                    entry.metadata().unwrap().modified().unwrap(),
                ));
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

#[test]
fn watcher_handles_partial_and_renamed_downloads_without_blocking_or_reimporting() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let downloads = root.join("downloads");
    fs::create_dir_all(&downloads).unwrap();
    let project = root.join("project/board.kicad_pro");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    fs::write(&project, "{}").unwrap();
    let config = root.join("config.toml");
    fs::write(&config, format!("[libraries]\npath = {:?}\nkicad_config = {:?}\n[watch]\ndirectories = [{:?}]\nsettle_seconds = 0\nretry_seconds = 2\n[destination]\npolicy = \"project\"\nproject = {:?}\n[matching]\nambiguous = \"separate\"\n[ui]\nenabled = false\n", root.join("global"), root.join("kicad"), downloads, project)).unwrap();
    let log = root.join("watch.log");
    let spawn = || {
        let logfile = fs::File::create(&log).unwrap();
        Watcher(
            Command::new(env!("CARGO_BIN_EXE_uniloader"))
                .args([
                    "--config",
                    config.to_str().unwrap(),
                    "--noninteractive",
                    "watch",
                ])
                .env("HOME", root.join("home"))
                .env("XDG_CONFIG_HOME", root.join("config"))
                .env("XDG_DATA_HOME", root.join("data"))
                .env("XDG_CACHE_HOME", root.join("cache"))
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("DBUS_SESSION_BUS_ADDRESS")
                .current_dir(root)
                .stdin(Stdio::null())
                .stdout(Stdio::from(logfile.try_clone().unwrap()))
                .stderr(Stdio::from(logfile))
                .spawn()
                .unwrap(),
        )
    };
    let watcher = spawn();
    until("watcher readiness", &log, || {
        fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("Watching")
    });
    // Directory scans emit Linux inotify OPEN events themselves. Idle watching
    // must block instead of repeatedly scanning in response to its own reads.
    #[cfg(target_os = "linux")]
    {
        let runtime = || {
            fs::read_to_string(format!("/proc/{}/schedstat", watcher.0.id()))
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<u64>()
                .unwrap()
        };
        let before = runtime();
        thread::sleep(Duration::from_secs(1));
        let cpu = Duration::from_nanos(runtime() - before);
        assert!(
            cpu < Duration::from_millis(200),
            "idle watcher spent {cpu:?} on CPU in one second"
        );
    }
    let first = archive("First", 1);
    let second = archive("Independent", 2);
    let third = archive("Renamed", 3);
    // A corrupt/incomplete final filename must not block an independent ZIP.
    fs::write(downloads.join("partial.zip"), &first[..20]).unwrap();
    fs::write(downloads.join("independent.zip"), &second).unwrap();
    // A completed .part is still not a component until its final rename.
    fs::write(downloads.join("renamed.zip.part"), &third).unwrap();
    until(
        "independent component despite partial archive",
        &log,
        || completed(root) == 1,
    );
    assert!(
        !root
            .join("project/libraries/UniLoader.pretty/First.kicad_mod")
            .exists()
    );
    assert!(
        !root
            .join("project/libraries/UniLoader.pretty/Renamed.kicad_mod")
            .exists()
    );
    fs::write(downloads.join("partial.zip.part"), &first).unwrap();
    fs::rename(
        downloads.join("partial.zip.part"),
        downloads.join("partial.zip"),
    )
    .unwrap();
    fs::rename(
        downloads.join("renamed.zip.part"),
        downloads.join("renamed.zip"),
    )
    .unwrap();
    until("both renamed components", &log, || completed(root) == 3);
    for (file, bytes) in [
        ("partial.zip", &first),
        ("independent.zip", &second),
        ("renamed.zip", &third),
    ] {
        assert_eq!(
            &fs::read(downloads.join(file)).unwrap(),
            bytes,
            "source archive must be preserved"
        );
    }
    let before = snapshot(root);
    // Duplicate creation and write events, including a new pathname for the
    // same archive content, must not edit libraries/history again.
    fs::write(downloads.join("duplicate.zip"), &first).unwrap();
    fs::write(downloads.join("independent.zip"), &second).unwrap();
    thread::sleep(Duration::from_millis(800));
    assert_eq!(completed(root), 3);
    assert_eq!(before, snapshot(root));
    drop(watcher);
    let restarted = spawn();
    until("watcher restart", &log, || {
        fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("Watching")
    });
    thread::sleep(Duration::from_millis(800));
    assert_eq!(completed(root), 3);
    assert_eq!(
        before,
        snapshot(root),
        "restart must not rewrite completed imports"
    );
    drop(restarted);
}
