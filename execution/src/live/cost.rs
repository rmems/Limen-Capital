//! Persistent post-mortem reservations, shared with the tick budget.
//! The ledger is scoped to a journal; keep the same journal across restarts.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    days: BTreeMap<String, BTreeMap<String, f64>>,
}

fn path(journal: &Path) -> PathBuf {
    let mut p = journal.as_os_str().to_os_string();
    p.push(".llm-cost.json");
    p.into()
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    s.into()
}

fn read_ledger(file: &mut std::fs::File) -> io::Result<Ledger> {
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    if text.is_empty() {
        return Ok(Ledger::default());
    }
    let ledger: Ledger = serde_json::from_str(&text).map_err(io::Error::other)?;
    if ledger
        .days
        .values()
        .flat_map(|d| d.values())
        .any(|v| !v.is_finite() || *v < 0.0)
    {
        return Err(io::Error::other("invalid LLM cost ledger"));
    }
    Ok(ledger)
}

// The sidecar is replaced atomically (temp + rename) so an interrupted write
// never leaves a partial ledger; the lock lives on a stable sibling file
// because a rename swaps the locked inode out from under a waiting process.
fn write_ledger(journal: &Path, ledger: &Ledger) -> io::Result<()> {
    let bytes = serde_json::to_vec(ledger)?;
    let tmp = sibling(&path(journal), ".tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path(journal))
}

fn update<T>(journal: &Path, f: impl FnOnce(&mut Ledger) -> io::Result<T>) -> io::Result<T> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(sibling(&path(journal), ".lock"))?;
    lock.lock()?;
    let mut ledger = if path(journal).exists() {
        read_ledger(&mut File::open(path(journal))?)?
    } else {
        Ledger::default()
    };
    let value = f(&mut ledger)?;
    write_ledger(journal, &ledger)?;
    Ok(value)
}

pub fn spent(journal: &Path, day: &str) -> io::Result<f64> {
    // A read by the tick loop does not create, lock, or rewrite the ledger.
    if !path(journal).exists() {
        return Ok(0.0);
    }
    let ledger = read_ledger(&mut File::open(path(journal))?)?;
    Ok(ledger
        .days
        .get(day)
        .map(|d| d.values().sum())
        .unwrap_or(0.0))
}

/// Journal spend already recorded plus the daily cap a reservation must fit under.
pub(crate) struct Budget {
    pub journal_spend: f64,
    pub cap: f64,
}

pub(crate) fn reserve(
    journal: &Path,
    day: &str,
    amount: f64,
    budget: Budget,
) -> io::Result<String> {
    update(journal, |l| {
        let entries = l.days.entry(day.into()).or_default();
        let total = entries.values().sum::<f64>() + budget.journal_spend + amount;
        if !total.is_finite() || total > budget.cap {
            return Err(io::Error::other(
                "max_llm_cost_usd_per_day would be exceeded",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        entries.insert(id.clone(), amount);
        Ok(id)
    })
}

pub(crate) fn settle(journal: &Path, day: &str, id: &str, amount: f64) -> io::Result<()> {
    update(journal, |l| {
        let entry = l
            .days
            .get_mut(day)
            .and_then(|d| d.get_mut(id))
            .ok_or_else(|| io::Error::other("missing LLM reservation"))?;
        // Record all known spend even if the provider exceeded its estimate.
        // Unknown/invalid usage and failed requests keep the reservation.
        if !amount.is_finite() || amount < 0.0 {
            return Err(io::Error::other("invalid LLM usage"));
        }
        *entry = amount;
        Ok(())
    })
}
