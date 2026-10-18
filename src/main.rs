// Copyright (C) 2026 Hector van der Aa <hector@h3cx.dev>
// SPDX-License-Identifier: GPL-3.0-or-later
mod config;
mod desktop;
mod destination;
mod doctor;
mod gui;
mod importer;
mod matching;
mod models;
mod preview;
mod sexpr;
mod sources;
mod storage;
mod symbols;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use config::Config;
use importer::{Request, Session};
use notify::{
    Event, EventKind, RecursiveMode, Watcher,
    event::{AccessKind, AccessMode},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant, SystemTime},
};
use storage::Record;

#[derive(Parser)]
#[command(
    version,
    about = "Import downloaded KiCad components into global or portable project libraries"
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    noninteractive: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}
#[derive(Subcommand)]
enum Commands {
    // Isolate the native event loop on its own main thread while the watcher
    // continues receiving downloads. This is the same executable, not a helper.
    #[command(name = "__dialog", hide = true)]
    Dialog,
    /// Watch configured download directories (also the default command).
    Watch,
    /// Import a ZIP archive or extracted component directory.
    Import {
        source: PathBuf,
        #[arg(long, conflicts_with = "project")]
        global: bool,
        #[arg(long)]
        project: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// List pending and failed imports; use --all for full history.
    Pending {
        #[arg(long)]
        all: bool,
    },
    /// Retry an import listed by pending.
    Retry {
        id: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Check libraries and paths; --fix recreates missing managed libraries and repairs references.
    Doctor {
        #[arg(long, required_unless_present = "project", conflicts_with = "project")]
        global: bool,
        #[arg(long)]
        project: Option<PathBuf>,
        #[arg(long)]
        fix: bool,
    },
    /// Show project suggestions from running KiCad processes.
    Discover,
}

fn execute(cfg: &Config, req: &Request, mut record: Record, session: &mut Session) -> Result<()> {
    match importer::run(cfg, req, &mut record, session) {
        Ok(()) => Ok(()),
        Err(error) => {
            let message = format!("{error:#}");
            if message.contains("another UniLoader import is active") {
                return Err(error);
            }
            record.status = if message.contains("pending:") {
                "pending"
            } else if message.contains("skipped:") {
                "skipped"
            } else {
                "failed"
            }
            .into();
            record.message = message.clone();
            if !req.dry_run {
                record.save()?;
            }
            bail!(
                "{}: {message} (import {}; retry with `uniloader retry {}`)",
                record.source.display(),
                record.id,
                record.id
            )
        }
    }
}
fn new_record(source: PathBuf, req: &Request) -> Result<Record> {
    let source = fs::canonicalize(source)?;
    let hash = storage::source_hash(&source)?;
    let mut record = Record::new(source, hash);
    if req.global || req.project.is_some() {
        let target = if req.global {
            "global".to_owned()
        } else {
            config::expand(req.project.as_ref().unwrap())
                .to_string_lossy()
                .into_owned()
        };
        record.id = format!("{}-{}", record.id, &storage::hash(target.as_bytes())[..8]);
    }
    Ok(record)
}
struct Waiting {
    size: u64,
    modified: Option<SystemTime>,
    changed: Instant,
    first: Instant,
}
fn zip_file(p: &Path) -> bool {
    p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}
fn complete_zip(p: &Path) -> Result<bool> {
    let mut zip = zip::ZipArchive::new(fs::File::open(p)?)?;
    let mut supported = false;
    for i in 0..zip.len() {
        let f = zip.by_index(i)?;
        let name = f.name().to_ascii_lowercase();
        if name.ends_with(".kicad_sym") || name.ends_with(".kicad_mod") {
            supported = true;
        }
    }
    Ok(supported)
}
fn event_requires_scan(event: &Event) -> bool {
    // Reading a watched directory emits OPEN events on Linux, including from
    // our own scans. Only a writer closing a file can make access relevant.
    event.need_rescan()
        || !matches!(event.kind, EventKind::Access(_))
        || matches!(
            event.kind,
            EventKind::Access(AccessKind::Close(AccessMode::Write))
        )
}
fn watch(cfg: Config, noninteractive: bool) -> Result<()> {
    let _watch_lock = storage::Lock::watcher()?;
    let (events_tx, events_rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        // Filter before queuing so reads cannot keep the receive loop awake.
        if event.as_ref().map_or(true, event_requires_scan) {
            let _ = events_tx.send(event);
        }
    })?;
    for dir in &cfg.watch.directories {
        if !dir.is_dir() {
            bail!(
                "watch directory {} does not exist; create it or configure watch.directories",
                dir.display()
            );
        }
        watcher.watch(dir, RecursiveMode::NonRecursive)?;
        println!("Watching {}", dir.display());
    }
    let (jobs_tx, jobs_rx) = mpsc::channel::<Record>();
    let (done_tx, done_rx) = mpsc::channel::<String>();
    let worker_cfg = cfg.clone();
    std::thread::spawn(move || {
        let mut session = Session::default();
        let req = Request {
            noninteractive,
            ..Request::default()
        };
        for record in jobs_rx {
            let id = record.id.clone();
            if let Err(e) = execute(&worker_cfg, &req, record, &mut session) {
                eprintln!("{e:#}");
                desktop::notify("Import needs attention. Run uniloader pending for details.");
            } else {
                desktop::notify("Component import finished. See the service log for details.");
            }
            let _ = done_tx.send(id);
        }
    });
    let mut waiting: BTreeMap<PathBuf, Waiting> = BTreeMap::new();
    let mut submitted: BTreeSet<String> = BTreeSet::new();
    let mut seen: BTreeMap<PathBuf, (u64, Option<SystemTime>)> = BTreeMap::new();
    // A restart resumes pending choices once, without endlessly reopening closed dialogs.
    for r in storage::records()? {
        if ["pending", "importing"].contains(&r.status.as_str()) && r.source.exists() {
            submitted.insert(r.id.clone());
            jobs_tx.send(r)?;
        }
    }
    let scan = |waiting: &mut BTreeMap<PathBuf, Waiting>,
                seen: &BTreeMap<PathBuf, (u64, Option<SystemTime>)>| {
        for dir in &cfg.watch.directories {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if !zip_file(&p) {
                        continue;
                    }
                    if let Ok(m) = entry.metadata() {
                        let signature = (m.len(), m.modified().ok());
                        if seen.get(&p) == Some(&signature) {
                            continue;
                        }
                        waiting
                            .entry(p)
                            .and_modify(|w| {
                                if (w.size, w.modified) != signature {
                                    w.size = signature.0;
                                    w.modified = signature.1;
                                    w.changed = Instant::now();
                                }
                            })
                            .or_insert(Waiting {
                                size: signature.0,
                                modified: signature.1,
                                changed: Instant::now(),
                                first: Instant::now(),
                            });
                    }
                }
            }
        }
    };
    scan(&mut waiting, &seen);
    let mut last_scan = Instant::now();
    loop {
        match events_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(event) => {
                let mut rescan = false;
                // Bound draining so sustained downloads cannot starve retries.
                for event in std::iter::once(event).chain(events_rx.try_iter().take(1023)) {
                    match event {
                        Ok(_) => rescan = true,
                        Err(e) => eprintln!("watch error: {e}"),
                    }
                }
                if rescan {
                    scan(&mut waiting, &seen);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("filesystem watcher disconnected"),
        }
        if last_scan.elapsed() >= Duration::from_secs(5) {
            scan(&mut waiting, &seen);
            last_scan = Instant::now();
        }
        for id in done_rx.try_iter() {
            submitted.remove(&id);
        }
        let ready = waiting
            .iter()
            .filter(|(_, w)| w.changed.elapsed() >= Duration::from_secs(cfg.watch.settle_seconds))
            .map(|(p, _)| p.clone())
            .collect::<Vec<_>>();
        for p in ready {
            if !p.exists() {
                waiting.remove(&p);
                continue;
            }
            match complete_zip(&p) {
                Ok(supported) => {
                    let w = waiting.remove(&p).unwrap();
                    seen.insert(p.clone(), (w.size, w.modified));
                    if !supported {
                        continue;
                    }
                    let req = Request::default();
                    match new_record(p.clone(), &req) {
                        Ok(r) => {
                            let history = storage::records()?;
                            // Completed files stay ignored until --force/manual import; pending
                            // files are resumed once at startup, not repeatedly every scan.
                            if history
                                .iter()
                                .any(|old| old.hash == r.hash && old.source == r.source)
                                || !submitted.insert(r.id.clone())
                            {
                                continue;
                            }
                            r.save()?;
                            jobs_tx.send(r)?;
                        }
                        Err(e) => eprintln!("{}: {e:#}", p.display()),
                    }
                }
                Err(e) => {
                    if waiting[&p].first.elapsed() >= Duration::from_secs(cfg.watch.retry_seconds) {
                        let w = waiting.remove(&p).unwrap();
                        seen.insert(p.clone(), (w.size, w.modified));
                        if let Ok(mut r) = new_record(p.clone(), &Request::default()) {
                            r.status = "failed".into();
                            r.message = format!("download never became a readable ZIP: {e:#}");
                            r.save()?;
                        }
                        eprintln!(
                            "{}: ZIP still unreadable after retry window: {e:#}",
                            p.display()
                        );
                    }
                }
            }
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Some(Commands::Dialog)) {
        return desktop::run_internal_dialog();
    }
    let cfg = Config::load(cli.config.as_deref())?;
    match cli.command.unwrap_or(Commands::Watch) {
        Commands::Dialog => unreachable!("handled before configuration loading"),
        Commands::Watch => watch(cfg, cli.noninteractive),
        Commands::Import {
            source,
            global,
            project,
            dry_run,
            force,
        } => {
            let req = Request {
                global,
                project,
                dry_run,
                noninteractive: cli.noninteractive,
                force,
            };
            let record = new_record(source, &req)?;
            execute(&cfg, &req, record, &mut Session::default())
        }
        Commands::Pending { all } => {
            for r in storage::records()? {
                if all || ["pending", "failed", "importing"].contains(&r.status.as_str()) {
                    println!(
                        "{} [{}] {}\n  {}\n  {}",
                        r.id,
                        r.status,
                        r.source.display(),
                        r.destination.as_deref().unwrap_or("destination not chosen"),
                        r.message
                    );
                    if all {
                        for p in &r.changed {
                            println!("  Changed: {}", p.display());
                        }
                        for p in &r.backups {
                            println!("  Backup: {}", p.display());
                        }
                    }
                }
            }
            Ok(())
        }
        Commands::Retry { id, dry_run } => {
            let record = storage::records()?
                .into_iter()
                .find(|r| r.id == id)
                .context("unknown import ID; run uniloader pending")?;
            let hash = storage::source_hash(&record.source)?;
            if hash != record.hash {
                bail!(
                    "source changed since queued; use `uniloader import` to review it as a new component"
                );
            }
            execute(
                &cfg,
                &Request {
                    dry_run,
                    noninteractive: cli.noninteractive,
                    force: true,
                    ..Request::default()
                },
                record,
                &mut Session::default(),
            )
        }
        Commands::Doctor {
            global: _,
            project,
            fix,
        } => doctor::run(&cfg, project.as_deref(), fix),
        Commands::Discover => {
            let p = desktop::projects();
            if p.is_empty() {
                println!("No project suggestions found. Browse or use --project.");
            } else {
                for path in p {
                    println!("{} (process suggestion)", path.display());
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    use notify::event::{CreateKind, Flag, ModifyKind, RemoveKind};

    #[test]
    fn scans_changes_and_write_closes_but_ignores_read_access() {
        for kind in [
            EventKind::Any,
            EventKind::Create(CreateKind::File),
            EventKind::Modify(ModifyKind::Any),
            EventKind::Remove(RemoveKind::File),
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
        ] {
            assert!(event_requires_scan(&Event::new(kind)), "{kind:?}");
        }
        for kind in [
            AccessKind::Any,
            AccessKind::Read,
            AccessKind::Open(AccessMode::Read),
            AccessKind::Open(AccessMode::Write),
            AccessKind::Close(AccessMode::Read),
        ] {
            let event = Event::new(EventKind::Access(kind));
            assert!(!event_requires_scan(&event), "{kind:?}");
            assert!(event_requires_scan(&event.set_flag(Flag::Rescan)));
        }
    }
}
