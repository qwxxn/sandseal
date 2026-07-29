//! Which sandboxes on this machine still have someone driving them.
//!
//! Every started sandbox writes a record under `~/.sandseal/instances/` and holds an exclusive
//! `flock` on it for as long as the CLI runs. The lock is the liveness signal, and it is the
//! kernel's to keep: it is released when the process ends, however it ended — a closed
//! terminal, `kill -9`, a crashed shell, a machine that lost power. So a record whose lock can
//! be taken belongs to a container nobody is attached to any more, and the collector in
//! `gc.rs` can say that without guessing at process ids, which get reused.
//!
//! The record also carries what reaping needs — the tmp dir to delete, the session to close —
//! because by the time anyone reads it, the process that knew those things is gone.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};
use tracing::debug;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InstanceRecord {
    /// Also the compose project name, which is how the collector finds the containers.
    pub instance_name: String,
    pub project_dir: PathBuf,
    /// Holds the compose override and the mounted prestart scripts. Nothing else owns it.
    pub tmp_dir: PathBuf,
    /// Present only when the session reached the backend, which the free tier never does.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The backend this session was opened against — a dev sandbox must not be closed
    /// against production.
    #[serde(default)]
    pub api_url: Option<String>,
    /// Unix seconds. For reporting only; nothing decides liveness by age.
    pub started_at: u64,
}

/// A held claim on an instance record. Keep it alive for as long as the sandbox runs.
pub struct InstanceClaim {
    /// Dropping this releases the lock. Never taken out except by `remove`.
    _lock: Flock<File>,
    path: PathBuf,
}

impl InstanceClaim {
    /// Drops the record on a clean shutdown, so the collector has nothing left to find.
    pub fn remove(self) {
        if let Err(err) = fs::remove_file(&self.path) {
            debug!("could not remove instance record {}: {err}", self.path.display());
        }
    }
}

/// An instance record nobody holds any more, and the lock proving it.
pub struct Orphan {
    pub record: InstanceRecord,
    /// Held for the lifetime of the sweep so two collectors cannot reap the same sandbox.
    _lock: Flock<File>,
    path: PathBuf,
}

impl Orphan {
    pub fn forget(self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn instances_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".sandseal/instances"))
}

/// Registers this sandbox and takes the lock that says it is alive.
pub fn claim(record: &InstanceRecord) -> Result<InstanceClaim> {
    let dir = instances_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create {}", dir.display()))?;

    let path = dir.join(format!("{}.json", record.instance_name));
    // Written under a staging name and renamed once complete AND locked. The lock lives on
    // the inode, so by the time the published path exists a collector opening it finds the
    // lock already taken — there is no window where a finished record looks abandoned.
    let staging = dir.join(format!(".{}.claiming", record.instance_name));

    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&staging)
        .with_context(|| format!("cannot write {}", staging.display()))?;

    let mut lock = Flock::lock(file, FlockArg::LockExclusiveNonblock)
        .map_err(|(_, errno)| anyhow!("cannot lock {}: {errno}", staging.display()))?;

    let body = serde_json::to_vec_pretty(record)?;
    lock.write_all(&body)?;
    lock.flush()?;

    fs::rename(&staging, &path)
        .with_context(|| format!("cannot publish {}", path.display()))?;

    debug!("claimed instance record {}", path.display());
    Ok(InstanceClaim { _lock: lock, path })
}

/// Every record whose owner is gone. Records still held are silently skipped.
pub fn orphans() -> Vec<Orphan> {
    let Ok(dir) = instances_dir() else { return Vec::new() };
    let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };

    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(orphan) = inspect(&path) {
            found.push(orphan);
        }
    }
    found
}

/// Takes the lock, or reports that someone else holds it. `None` means "leave this alone".
fn inspect(path: &Path) -> Option<Orphan> {
    let file = File::open(path).ok()?;

    let lock = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(lock) => lock,
        // EWOULDBLOCK: a live CLI holds it. Anything else (a filesystem without flock, say)
        // is treated the same way, because reaping on a failed liveness check is the one
        // outcome worth ruling out.
        Err((_, errno)) => {
            debug!("instance {} is alive ({errno})", path.display());
            return None;
        }
    };

    let body = fs::read_to_string(path).ok()?;
    match serde_json::from_str::<InstanceRecord>(&body) {
        Ok(record) => Some(Orphan { record, _lock: lock, path: path.to_path_buf() }),
        Err(err) => {
            // A record from a version that wrote a different shape, or one truncated by a
            // crash. Nothing here can be acted on, and leaving it means finding it again on
            // every sweep forever.
            debug!("discarding unreadable instance record {}: {err}", path.display());
            let _ = fs::remove_file(path);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, tmp: &Path) -> InstanceRecord {
        InstanceRecord {
            instance_name: name.to_string(),
            project_dir: PathBuf::from("/home/me/project"),
            tmp_dir: tmp.to_path_buf(),
            session_id: Some("sess_1".into()),
            api_url: Some("https://sandseal.io".into()),
            started_at: 1_700_000_000,
        }
    }

    /// `claim` writes to the real home, so the round trip is exercised through the same
    /// primitives against a temp dir instead.
    fn claim_at(dir: &Path, record: &InstanceRecord) -> (Flock<File>, PathBuf) {
        let path = dir.join(format!("{}.json", record.instance_name));
        let file = OpenOptions::new().create(true).truncate(true).write(true).open(&path).unwrap();
        let mut lock = Flock::lock(file, FlockArg::LockExclusiveNonblock).ok().unwrap();
        lock.write_all(&serde_json::to_vec_pretty(record).unwrap()).unwrap();
        lock.flush().unwrap();
        (lock, path)
    }

    #[test]
    fn a_held_record_is_not_an_orphan() {
        let dir = tempfile::tempdir().unwrap();
        let (_held, path) = claim_at(dir.path(), &record("live", Path::new("/tmp/x")));

        assert!(inspect(&path).is_none(), "a locked record must look alive");
    }

    #[test]
    fn a_released_record_is_an_orphan_and_keeps_what_reaping_needs() {
        let dir = tempfile::tempdir().unwrap();
        let (held, path) = claim_at(dir.path(), &record("dead", Path::new("/tmp/dead-tmp")));
        // What the kernel does when the owning process ends.
        drop(held);

        let orphan = inspect(&path).expect("a released record must be reapable");
        assert_eq!(orphan.record.instance_name, "dead");
        assert_eq!(orphan.record.tmp_dir, Path::new("/tmp/dead-tmp"));
        assert_eq!(orphan.record.session_id.as_deref(), Some("sess_1"));
    }

    #[test]
    fn an_unreadable_record_is_dropped_rather_than_found_forever() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.json");
        fs::write(&path, b"{ truncated").unwrap();

        assert!(inspect(&path).is_none());
        assert!(!path.exists(), "an unusable record must not survive the sweep");
    }

    #[test]
    fn a_record_survives_the_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let original = record("trip", Path::new("/tmp/trip"));
        let (held, path) = claim_at(dir.path(), &original);
        drop(held);

        let orphan = inspect(&path).unwrap();
        assert_eq!(orphan.record.api_url.as_deref(), Some("https://sandseal.io"));
        assert_eq!(orphan.record.project_dir, Path::new("/home/me/project"));
        assert_eq!(orphan.record.started_at, 1_700_000_000);

        orphan.forget();
        assert!(!path.exists());
    }
}
