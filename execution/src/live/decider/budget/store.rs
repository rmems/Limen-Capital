//! The default [`BudgetStore`](super::BudgetStore): a JSON file replaced
//! atomically (write temp, fsync, rename, fsync dir) and guarded by an OS
//! lock so two processes cannot meter the same budget.

use super::{BudgetStore, LedgerState};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub struct FileStore {
    path: PathBuf,
    _lock: File,
}

impl fmt::Debug for FileStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileStore")
            .field("path", &self.path)
            .finish()
    }
}

pub(crate) fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

impl FileStore {
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(sibling(&path, ".lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                return Err(io::Error::other(format!(
                    "{} is in use by another process",
                    path.display()
                )));
            }
            Err(fs::TryLockError::Error(e)) => return Err(e),
        }
        Ok(Self { path, _lock: lock })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl BudgetStore for FileStore {
    fn load(&mut self) -> io::Result<Option<LedgerState>> {
        match fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn save(&mut self, state: &LedgerState) -> io::Result<()> {
        let tmp = sibling(&self.path, ".tmp");
        let bytes = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
        write_synced(&tmp, &bytes)?;
        fs::rename(&tmp, &self.path)?;
        sync_parent_dir(&self.path)
    }
}

/// Write all bytes and fsync before the caller renames over the ledger.
fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// fsync the directory so the rename survives a crash. Windows cannot open a
/// directory as a `File` through std, so the directory sync is skipped there;
/// the file itself is still fsynced before the rename.
#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}
