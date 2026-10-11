use crate::config;
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn source_hash(path: &Path) -> Result<String> {
    if path.is_file() {
        return Ok(hash(&fs::read(path)?));
    }
    let mut h = Sha256::new();
    for e in walkdir::WalkDir::new(path)
        .follow_links(false)
        .sort_by_file_name()
    {
        let e = e?;
        if e.file_type().is_file() {
            h.update(e.path().strip_prefix(path)?.to_string_lossy().as_bytes());
            h.update([0]);
            h.update(fs::read(e.path())?);
        }
    }
    Ok(format!("{:x}", h.finalize()))
}
pub struct Lock {
    _file: File,
}
impl Lock {
    pub fn acquire() -> Result<Self> {
        Self::named("writer.lock")
    }
    pub fn watcher() -> Result<Self> {
        Self::named("watcher.lock")
    }
    fn named(name: &str) -> Result<Self> {
        fs::create_dir_all(config::data_dir())?;
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(config::data_dir().join(name))?;
        f.try_lock_exclusive().with_context(|| {
            if name == "writer.lock" {
                "another UniLoader import is active; retry shortly"
            } else {
                "another UniLoader watcher is already running"
            }
        })?;
        Ok(Self { _file: f })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub source: PathBuf,
    pub hash: String,
    pub status: String,
    pub destination: Option<String>,
    pub project: Option<PathBuf>,
    pub message: String,
    #[serde(default)]
    pub changed: Vec<PathBuf>,
    #[serde(default)]
    pub backups: Vec<PathBuf>,
}
impl Record {
    pub fn new(source: PathBuf, hash: String) -> Self {
        let id = hash[..16].to_owned();
        Self {
            id,
            source,
            hash,
            status: "pending".into(),
            destination: None,
            project: None,
            message: String::new(),
            changed: vec![],
            backups: vec![],
        }
    }
    pub fn save(&self) -> Result<()> {
        let dir = config::data_dir().join("imports");
        fs::create_dir_all(&dir)?;
        atomic(
            &dir.join(format!("{}.json", self.id)),
            &serde_json::to_vec_pretty(self)?,
        )
    }
}
pub fn records() -> Result<Vec<Record>> {
    let d = config::data_dir().join("imports");
    if !d.exists() {
        return Ok(vec![]);
    }
    let mut out: Vec<Record> = vec![];
    for e in fs::read_dir(d)? {
        let p = e?.path();
        if p.extension().is_some_and(|e| e == "json") {
            out.push(
                serde_json::from_slice(&fs::read(&p)?)
                    .with_context(|| format!("invalid history {}", p.display()))?,
            );
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}
pub fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("missing parent")?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
#[derive(Default)]
pub struct Plan {
    pub writes: BTreeMap<PathBuf, Vec<u8>>,
    pub messages: Vec<String>,
    pub directories: Vec<PathBuf>,
}
impl Plan {
    pub fn put(&mut self, path: PathBuf, bytes: Vec<u8>) -> Result<()> {
        if let Some(old) = self.writes.get(&path)
            && old != &bytes
        {
            bail!("conflicting staged writes to {}", path.display());
        }
        self.writes.insert(path, bytes);
        Ok(())
    }
    pub fn commit(&self, record: &mut Record) -> Result<()> {
        for dir in &self.directories {
            fs::create_dir_all(dir)?;
        }
        let backup_root = config::data_dir().join("backups").join(format!(
            "{}-{}",
            record.id,
            chrono::Utc::now().timestamp_millis()
        ));
        for (p, bytes) in &self.writes {
            if p.exists() {
                let old = fs::read(p).with_context(|| format!("reading target {}", p.display()))?;
                if old == *bytes {
                    continue;
                }
                fs::create_dir_all(&backup_root)?;
                let backup = backup_root.join(format!(
                    "{}-{}",
                    record.backups.len(),
                    p.file_name().unwrap_or_default().to_string_lossy()
                ));
                fs::write(&backup, old)?;
                record.backups.push(backup);
            }
            record.message = format!("writing {}", p.display());
            record.save()?;
            atomic(p, bytes)?;
            record.changed.push(p.clone());
            record.save()?;
        }
        Ok(())
    }
}
