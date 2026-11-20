//! Stops: single-instance lock, operator halt file, and Ctrl-C.
//!
//! * [`InstanceLock`] holds an exclusive OS file lock in the state directory
//!   for the life of the process, so two `limen-live` processes cannot trade
//!   the same account (one state directory per account).
//! * [`HaltFile`]: an operator `touch`es it; the loop cancels resting orders
//!   and stops, and refuses to start again until the file is removed.
//! * [`StopSignal`]: Ctrl-C observed at any point of a tick, not only while
//!   the loop sleeps.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Debug)]
pub struct InstanceLock {
    path: PathBuf,
    _file: File,
}

impl InstanceLock {
    /// Take the lock or fail naming the path (another instance holds it).
    pub fn acquire(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "another limen-live instance holds {}; refusing to trade the same account twice",
                        path.display()
                    ),
                ))
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
        // Holder pid for the operator; the lock itself is the OS lock, not this text.
        file.set_len(0)?;
        writeln!(file, "{}", std::process::id())?;
        Ok(Self { path, _file: file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone)]
pub struct HaltFile {
    path: PathBuf,
}

impl HaltFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_set(&self) -> bool {
        self.path.exists()
    }
}

/// Ctrl-C latch shared between a watcher task and the loop.
#[derive(Debug, Clone, Default)]
pub struct StopSignal {
    stopped: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl StopSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn trigger(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    pub fn is_triggered(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Resolves once triggered (immediately if it already was).
    pub async fn wait(&self) {
        while !self.is_triggered() {
            self.notify.notified().await;
        }
    }

    /// Spawn a task that triggers on Ctrl-C. Abort the handle when done.
    pub fn watch_ctrl_c(&self) -> tokio::task::JoinHandle<()> {
        let signal = self.clone();
        tokio::spawn(async move {
            match tokio::signal::ctrl_c().await {
                Ok(()) => signal.trigger(),
                Err(e) => tracing::warn!("ctrl-c handler unavailable: {e}"),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_instance_is_refused_until_the_first_exits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/limen-live.lock");
        let first = InstanceLock::acquire(&path).unwrap();
        let err = InstanceLock::acquire(&path).unwrap_err();
        assert!(
            err.to_string().contains("another limen-live instance"),
            "{err}"
        );
        drop(first);
        let again = InstanceLock::acquire(&path).unwrap();
        assert_eq!(again.path(), path);
    }

    #[test]
    fn halt_file_is_a_touch() {
        let dir = tempfile::tempdir().unwrap();
        let halt = HaltFile::new(dir.path().join("HALT"));
        assert!(!halt.is_set());
        std::fs::write(halt.path(), "").unwrap();
        assert!(halt.is_set());
    }

    #[tokio::test]
    async fn stop_signal_latches() {
        let s = StopSignal::new();
        assert!(!s.is_triggered());
        let waiter = tokio::spawn({
            let s = s.clone();
            async move { s.wait().await }
        });
        s.trigger();
        waiter.await.unwrap();
        s.wait().await;
        assert!(s.is_triggered());
    }
}
