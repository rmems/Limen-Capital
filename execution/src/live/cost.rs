//! Persistent post-mortem reservations, shared with the tick budget.
//! The ledger is scoped to a journal; keep the same journal across restarts.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, Read, Seek, Write};
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

fn update<T>(journal: &Path, f: impl FnOnce(&mut Ledger) -> io::Result<T>) -> io::Result<T> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path(journal))?;
    file.lock()?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let mut ledger: Ledger = if text.is_empty() {
        Ledger::default()
    } else {
        serde_json::from_str(&text).map_err(io::Error::other)?
    };
    if ledger
        .days
        .values()
        .flat_map(|d| d.values())
        .any(|v| !v.is_finite() || *v < 0.0)
    {
        return Err(io::Error::other("invalid LLM cost ledger"));
    }
    let value = f(&mut ledger)?;
    let bytes = serde_json::to_vec(&ledger)?;
    file.rewind()?;
    file.write_all(&bytes)?;
    file.set_len(bytes.len() as u64)?;
    file.sync_all()?;
    Ok(value)
}

pub fn spent(journal: &Path, day: &str) -> io::Result<f64> {
    // A read by the tick loop does not create or rewrite an absent ledger.
    if !path(journal).exists() {
        return Ok(0.0);
    }
    update(journal, |l| {
        Ok(l.days.get(day).map(|d| d.values().sum()).unwrap_or(0.0))
    })
}

pub(crate) fn reserve(
    journal: &Path,
    day: &str,
    amount: f64,
    journal_spend: f64,
    cap: f64,
) -> io::Result<String> {
    update(journal, |l| {
        let entries = l.days.entry(day.into()).or_default();
        let total = entries.values().sum::<f64>() + journal_spend + amount;
        if !total.is_finite() || total > cap {
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
