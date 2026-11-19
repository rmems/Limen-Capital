//! Append-only markdown journal (`journal.md`).
//!
//! One table row per tick: state summary, decision + reason, risk verdict,
//! order id/status, fill, PnL and LLM cost. The file is created with a header
//! block (mode, source, symbol, caps, disclaimer) the first time it is written.
//! `tail(n)` returns the last `n` rows for the decider prompt.

use crate::live::risk::RiskLimits;
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// How many recent rows are kept in memory for `tail`.
pub const RECENT_ROWS: usize = 32;

pub const COLUMNS: &[&str] = &[
    "ts (UTC)",
    "tick",
    "symbol",
    "bid",
    "ask",
    "mid",
    "spread bps",
    "pos qty",
    "pos $",
    "cash $",
    "realized $",
    "decider",
    "action",
    "size $",
    "reason",
    "risk",
    "order",
    "fill",
    "pnl $",
    "llm $",
    "llm calls",
];

/// Run metadata written once at the top of a fresh journal.
#[derive(Debug, Clone)]
pub struct JournalHeader {
    pub started_utc: String,
    pub mode: String,
    pub market_source: String,
    pub symbol: String,
    pub decider: String,
    pub model: Option<String>,
    pub limits: RiskLimits,
    /// LLM budget line (Claude decider only).
    pub llm: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct JournalRow {
    pub ts_utc: String,
    pub tick: u64,
    pub symbol: String,
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub spread_bps: f64,
    pub position_qty: f64,
    pub position_usd: f64,
    pub cash_usd: f64,
    pub realized_pnl_usd: f64,
    pub decider: String,
    pub action: String,
    pub size_usd: f64,
    pub reason: String,
    pub risk: String,
    /// `id:state` or `-`.
    pub order: String,
    /// `qty @ price` or `-`.
    pub fill: String,
    /// Realized PnL *this tick*.
    pub pnl_usd: f64,
    pub llm_cost_usd: f64,
    /// Per-call audit (role, model, usage, cost, latency, outcome). Last
    /// column, so prompts can drop it.
    pub llm_calls: String,
}

fn cell(s: &str) -> String {
    let one_line: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    one_line.replace('|', "\\|").trim().to_string()
}

fn dash_if_empty(s: &str) -> String {
    if s.trim().is_empty() {
        "-".to_string()
    } else {
        cell(s)
    }
}

impl JournalRow {
    pub fn to_markdown(&self) -> String {
        let cells = [
            cell(&self.ts_utc),
            self.tick.to_string(),
            cell(&self.symbol),
            format!("{:.2}", self.bid),
            format!("{:.2}", self.ask),
            format!("{:.2}", self.mid),
            format!("{:.2}", self.spread_bps),
            format!("{:.8}", self.position_qty),
            format!("{:.4}", self.position_usd),
            format!("{:.4}", self.cash_usd),
            format!("{:.4}", self.realized_pnl_usd),
            dash_if_empty(&self.decider),
            dash_if_empty(&self.action),
            format!("{:.4}", self.size_usd),
            dash_if_empty(&self.reason),
            dash_if_empty(&self.risk),
            dash_if_empty(&self.order),
            dash_if_empty(&self.fill),
            format!("{:+.4}", self.pnl_usd),
            format!("{:.6}", self.llm_cost_usd),
            dash_if_empty(&self.llm_calls),
        ];
        format!("| {} |", cells.join(" | "))
    }
}

#[derive(Debug, Clone)]
pub struct Journal {
    path: PathBuf,
    /// Last [`RECENT_ROWS`] rows, oldest first. Seeded from the file by
    /// `ensure_header`, then maintained by `append`, so a tick never re-reads
    /// the whole (growing) file.
    recent: VecDeque<String>,
    loaded: bool,
}

impl Journal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            recent: VecDeque::with_capacity(RECENT_ROWS),
            loaded: false,
        }
    }

    fn is_data_row(line: &str) -> bool {
        line.starts_with("| ") && !line.starts_with("| ts (UTC)")
    }

    fn remember(&mut self, row: String) {
        if self.recent.len() == RECENT_ROWS {
            self.recent.pop_front();
        }
        self.recent.push_back(row);
    }

    /// Read the existing file once and keep its last rows in memory.
    fn load_recent(&mut self) -> io::Result<()> {
        if self.loaded {
            return Ok(());
        }
        match fs::read_to_string(&self.path) {
            Ok(text) => {
                for line in text.lines().filter(|l| Self::is_data_row(l)) {
                    self.remember(line.to_string());
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        self.loaded = true;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn is_fresh(&self) -> bool {
        match fs::metadata(&self.path) {
            Ok(m) => m.len() == 0,
            Err(_) => true,
        }
    }

    fn header_markdown(h: &JournalHeader) -> String {
        let l = &h.limits;
        let model = h.model.as_deref().unwrap_or("-");
        let mut s = String::new();
        s.push_str("# limen-live journal\n\n");
        s.push_str(&format!("- started: {}\n", h.started_utc));
        s.push_str(&format!(
            "- mode: **{}** | market: {} | symbol: {} | decider: {} | model: {}\n",
            h.mode, h.market_source, h.symbol, h.decider, model
        ));
        s.push_str(&format!(
            "- caps: max_position_usd={} max_order_usd={} max_daily_loss_usd={} max_llm_cost_usd_per_day={} min_tick_interval_secs={} max_open_orders={} stale_order_ticks={}\n",
            l.max_position_usd,
            l.max_order_usd,
            l.max_daily_loss_usd,
            l.max_llm_cost_usd_per_day,
            l.min_tick_interval_secs,
            l.max_open_orders,
            l.stale_order_ticks
        ));
        if let Some(llm) = &h.llm {
            s.push_str(&format!("- llm: {llm}\n"));
        }
        s.push_str("- limit orders only (buy @ bid, sell @ ask); no leverage; no shorts; one instrument.\n");
        s.push_str(
            "- **Not a research result.** Journal/LLM output never enters any training set.\n\n",
        );
        s.push_str(&format!("| {} |\n", COLUMNS.join(" | ")));
        s.push_str(&format!(
            "|{}\n",
            COLUMNS.iter().map(|_| "---|").collect::<String>()
        ));
        s
    }

    /// Create the file with its header if it does not exist or is empty.
    pub fn ensure_header(&mut self, header: &JournalHeader) -> io::Result<()> {
        self.load_recent()?;
        self.ensure_parent()?;
        if self.is_fresh() {
            self.write_header(header)
        } else {
            self.write_run_marker(header)
        }
    }

    fn ensure_parent(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        Ok(())
    }

    fn write_header(&self, header: &JournalHeader) -> io::Result<()> {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(Self::header_markdown(header).as_bytes())?;
        f.flush()?;
        Ok(())
    }

    fn write_run_marker(&mut self, header: &JournalHeader) -> io::Result<()> {
        // Appending to an existing journal: mark the new run.
        let marker = format!(
                "| {} | 0 | {} | 0.00 | 0.00 | 0.00 | 0.00 | 0.00000000 | 0.0000 | 0.0000 | 0.0000 | - | run_start | 0.0000 | mode={} market={} decider={} | - | - | - | +0.0000 | 0.000000 | - |",
                cell(&header.started_utc),
                cell(&header.symbol),
                cell(&header.mode),
                cell(&header.market_source),
                cell(&header.decider)
            );
        let mut f = OpenOptions::new().append(true).open(&self.path)?;
        writeln!(f, "{marker}")?;
        self.remember(marker);
        Ok(())
    }

    pub fn append(&mut self, row: &JournalRow) -> io::Result<()> {
        let line = row.to_markdown();
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{line}")?;
        f.flush()?;
        self.remember(line);
        Ok(())
    }

    /// Last `n` (≤ [`RECENT_ROWS`]) data rows, oldest first, from memory.
    pub fn tail(&mut self, n: usize) -> io::Result<Vec<String>> {
        self.load_recent()?;
        let start = self.recent.len().saturating_sub(n);
        Ok(self.recent.iter().skip(start).cloned().collect())
    }

    /// Total data rows on disk (scans the file; for tests / summaries only).
    pub fn row_count(&self) -> io::Result<usize> {
        match fs::read_to_string(&self.path) {
            Ok(text) => Ok(text.lines().filter(|l| Self::is_data_row(l)).count()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> JournalHeader {
        JournalHeader {
            started_utc: "2026-11-09T00:00:00Z".into(),
            mode: "paper".into(),
            market_source: "synthetic".into(),
            symbol: "BTC-USD".into(),
            decider: "rules".into(),
            model: None,
            limits: RiskLimits::default(),
            llm: None,
        }
    }

    fn row(tick: u64, reason: &str) -> JournalRow {
        JournalRow {
            ts_utc: "2026-11-09T00:00:01Z".into(),
            tick,
            symbol: "BTC-USD".into(),
            bid: 100.0,
            ask: 100.1,
            mid: 100.05,
            spread_bps: 10.0,
            position_qty: 0.0,
            position_usd: 0.0,
            cash_usd: 10.0,
            realized_pnl_usd: 0.0,
            decider: "rules".into(),
            action: "hold".into(),
            size_usd: 0.0,
            reason: reason.into(),
            risk: "hold".into(),
            order: String::new(),
            fill: String::new(),
            pnl_usd: 0.0,
            llm_cost_usd: 0.0,
            llm_calls: String::new(),
        }
    }

    #[test]
    fn header_then_rows_then_tail() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("basic.md");
        let mut j = Journal::new(&p);
        j.ensure_header(&header()).unwrap();
        j.append(&row(1, "a")).unwrap();
        j.append(&row(2, "b")).unwrap();
        j.append(&row(3, "c")).unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert!(text.starts_with("# limen-live journal"));
        assert!(text.contains("Not a research result"));
        assert!(text.contains("max_order_usd=5"));
        assert_eq!(text.lines().filter(|l| l.starts_with("|---|")).count(), 1);
        assert_eq!(j.row_count().unwrap(), 3);
        let t = j.tail(2).unwrap();
        assert_eq!(t.len(), 2);
        assert!(t[0].contains("| 2 |"));
        assert!(t[1].contains("| 3 |"));
        // Header is idempotent on a non-empty file: a run_start marker row is added instead.
        j.ensure_header(&header()).unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert_eq!(text.matches("# limen-live journal").count(), 1);
        assert!(text.contains("run_start"));
        assert_eq!(j.row_count().unwrap(), 4);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn cells_are_single_line_and_pipe_safe() {
        let r = row(1, "a | b\nc");
        let md = r.to_markdown();
        assert_eq!(md.lines().count(), 1);
        assert!(md.contains("a \\| b c"));
        assert_eq!(md.matches(" | ").count(), COLUMNS.len() - 1);
        assert!(md.ends_with(" | 0.000000 | - |"));
        assert!(md.contains("| - | - |")); // empty order/fill become dashes
    }

    #[test]
    fn tail_on_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("missing.md");
        assert!(Journal::new(&p).tail(5).unwrap().is_empty());
    }

    #[test]
    fn tail_is_served_from_memory_and_reloads_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("memory.md");
        let mut j = Journal::new(&p);
        j.ensure_header(&header()).unwrap();
        for i in 1..=(RECENT_ROWS as u64 + 5) {
            j.append(&row(i, "r")).unwrap();
        }
        let t = j.tail(3).unwrap();
        assert!(t[2].contains(&format!("| {} |", RECENT_ROWS + 5)));
        assert_eq!(j.tail(usize::MAX).unwrap().len(), RECENT_ROWS);
        assert_eq!(j.row_count().unwrap(), RECENT_ROWS + 5);
        // A fresh handle on the same file sees the last rows without a header rewrite.
        let mut j2 = Journal::new(&p);
        j2.ensure_header(&header()).unwrap();
        let t2 = j2.tail(2).unwrap();
        assert!(t2[0].contains(&format!("| {} |", RECENT_ROWS + 5)));
        assert!(t2[1].contains("run_start"));
        let _ = fs::remove_file(&p);
    }
}
