//! Persistent, reservation-based meter for this experiment's Claude spend.
//!
//! Invariants (all enforced under one lock, then written to disk before the
//! caller may dispatch):
//!
//! * committed + outstanding reservations + the new reservation never exceed
//!   the cumulative cap, the role's share, today's pacing cap, or the role's
//!   share of today. Day keys roll over; the cumulative total never does.
//! * No reservation is granted at or after the credit-expiry cutoff, or when
//!   the call could still be running at the cutoff.
//! * The stored cap and expiry can only move down. A config that raises the
//!   cap or extends the deadline is refused, not silently adopted.
//! * Reservations left pending by a crash become `ambiguous` on restart and
//!   stay counted until an operator reconciles them by hand.
//! * Missing usage, unknown pricing, a charge above its reservation, or a
//!   failed write halts all new paid calls (until an operator clears `halted`).

use crate::live::config::{ConfigError, LiveConfig};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

mod store;
#[cfg(test)]
pub(crate) use store::sibling;
pub use store::FileStore;

pub const LEDGER_VERSION: u32 = 1;
const EPS: f64 = 1e-12;

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(Utc::now)
}

pub fn day_key(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d").to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Decider,
    Helper,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Decider => "decider",
            Role::Helper => "helper",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleTotals {
    pub decider: f64,
    pub helper: f64,
}

impl RoleTotals {
    pub fn get(&self, role: Role) -> f64 {
        match role {
            Role::Decider => self.decider,
            Role::Helper => self.helper,
        }
    }

    fn add(&mut self, role: Role, v: f64) {
        match role {
            Role::Decider => self.decider += v,
            Role::Helper => self.helper += v,
        }
    }

    pub fn total(&self) -> f64 {
        self.decider + self.helper
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReservationState {
    /// Request in flight in this process.
    Pending,
    /// Outcome unknown (timeout, dropped stream, crash); counted until reconciled.
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reservation {
    pub id: u64,
    pub role: Role,
    pub model: String,
    pub day: String,
    pub amount_usd: f64,
    pub created_unix: i64,
    pub state: ReservationState,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleCounts {
    pub decider: u64,
    pub helper: u64,
}

/// On-disk ledger state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerState {
    pub version: u32,
    pub cap_total_usd: f64,
    pub expiry_unix: i64,
    pub committed_usd: RoleTotals,
    pub committed_by_day: BTreeMap<String, RoleTotals>,
    pub settled_calls: RoleCounts,
    pub reservations: Vec<Reservation>,
    pub next_id: u64,
    pub halted: Option<String>,
}

impl LedgerState {
    fn fresh(policy: &BudgetPolicy) -> Self {
        Self {
            version: LEDGER_VERSION,
            cap_total_usd: policy.cap_total_usd,
            expiry_unix: policy.expiry.timestamp(),
            committed_usd: RoleTotals::default(),
            committed_by_day: BTreeMap::new(),
            settled_calls: RoleCounts::default(),
            reservations: Vec::new(),
            next_id: 1,
            halted: None,
        }
    }

    fn reserved(&self, keep: impl Fn(&Reservation) -> bool) -> f64 {
        self.reservations
            .iter()
            .filter(|r| keep(r))
            .map(|r| r.amount_usd)
            .sum()
    }

    fn day_committed(&self, day: &str) -> RoleTotals {
        self.committed_by_day.get(day).copied().unwrap_or_default()
    }

    /// Committed plus outstanding, all roles.
    pub fn used_total_usd(&self) -> f64 {
        self.committed_usd.total() + self.reserved(|_| true)
    }

    pub fn used_day_usd(&self, day: &str) -> f64 {
        self.day_committed(day).total() + self.reserved(|r| r.day == day)
    }
}

/// Limits the ledger enforces, derived from validated config.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetPolicy {
    pub cap_total_usd: f64,
    pub cap_per_day_usd: f64,
    /// Helper share of the totals (0 when no helper is configured).
    pub helper_total_usd: f64,
    pub helper_per_day_usd: f64,
    pub expiry: DateTime<Utc>,
}

impl BudgetPolicy {
    pub fn from_config(cfg: &LiveConfig) -> Result<Self, ConfigError> {
        let (helper_total_usd, helper_per_day_usd) = match cfg.helper_model {
            Some(_) => cfg.llm.helper_caps()?,
            None => (0.0, 0.0),
        };
        Ok(Self {
            cap_total_usd: cfg.llm.budget_total_usd()?,
            cap_per_day_usd: cfg.risk.max_llm_cost_usd_per_day,
            helper_total_usd,
            helper_per_day_usd,
            expiry: cfg.llm.credit_expiry_utc()?,
        })
    }

    /// `(cumulative, per-day)` limits for `role`. The two shares partition the
    /// totals, so helper spend can never consume the decider's allocation.
    pub fn role_caps(&self, role: Role) -> (f64, f64) {
        match role {
            Role::Decider => (
                self.cap_total_usd - self.helper_total_usd,
                self.cap_per_day_usd - self.helper_per_day_usd,
            ),
            Role::Helper => (self.helper_total_usd, self.helper_per_day_usd),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BudgetError {
    Halted(String),
    Expired {
        expiry_utc: String,
    },
    Exhausted {
        scope: String,
        used_usd: f64,
        request_usd: f64,
        limit_usd: f64,
    },
    Persistence(String),
    Invalid(String),
    CapRaised {
        stored_usd: f64,
        requested_usd: f64,
    },
    ExpiryExtended {
        stored_utc: String,
        requested_utc: String,
    },
    Corrupt(String),
}

impl fmt::Display for BudgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BudgetError::Halted(r) => write!(f, "paid calls halted: {r}"),
            BudgetError::Expired { expiry_utc } => {
                write!(f, "credit expiry {expiry_utc} reached: no new paid calls")
            }
            BudgetError::Exhausted {
                scope,
                used_usd,
                request_usd,
                limit_usd,
            } => write!(
                f,
                "{scope} budget: used ${used_usd:.6} + reserve ${request_usd:.6} > ${limit_usd:.6}"
            ),
            BudgetError::Persistence(m) => write!(f, "ledger persistence failed: {m}"),
            BudgetError::Invalid(m) => write!(f, "invalid reservation: {m}"),
            BudgetError::CapRaised {
                stored_usd,
                requested_usd,
            } => write!(
                f,
                "config raises the experiment cap from ${stored_usd} to ${requested_usd}; the cumulative cap can only be lowered"
            ),
            BudgetError::ExpiryExtended {
                stored_utc,
                requested_utc,
            } => write!(
                f,
                "config moves credit expiry from {stored_utc} to later {requested_utc}; the deadline cannot be extended"
            ),
            BudgetError::Corrupt(m) => write!(f, "ledger unreadable (refusing to reset): {m}"),
        }
    }
}

impl std::error::Error for BudgetError {}

/// Where ledger state lives. The JSON file store is the default; the durable
/// book ledger (RM-2462) can implement this to hold the same state.
pub trait BudgetStore: Send {
    fn load(&mut self) -> io::Result<Option<LedgerState>>;
    fn save(&mut self, state: &LedgerState) -> io::Result<()>;
}

struct Inner {
    state: LedgerState,
    policy: BudgetPolicy,
    store: Box<dyn BudgetStore>,
}

impl Inner {
    fn persist(&mut self) -> Result<(), BudgetError> {
        self.store.save(&self.state).map_err(|e| {
            self.state
                .halted
                .get_or_insert_with(|| "persistence_failed".into());
            BudgetError::Persistence(e.to_string())
        })
    }

    fn check_limit(scope: String, used: f64, request: f64, limit: f64) -> Result<(), BudgetError> {
        if used + request > limit + EPS {
            return Err(BudgetError::Exhausted {
                scope,
                used_usd: used,
                request_usd: request,
                limit_usd: limit,
            });
        }
        Ok(())
    }

    fn check_affordable(&self, role: Role, day: &str, amount: f64) -> Result<(), BudgetError> {
        let s = &self.state;
        let p = &self.policy;
        let (role_total, role_day) = p.role_caps(role);
        let day_role =
            s.day_committed(day).get(role) + s.reserved(|r| r.day == day && r.role == role);
        Self::check_limit(
            "cumulative".into(),
            s.used_total_usd(),
            amount,
            s.cap_total_usd,
        )?;
        Self::check_limit(
            format!("{role} cumulative"),
            s.committed_usd.get(role) + s.reserved(|r| r.role == role),
            amount,
            role_total,
        )?;
        Self::check_limit(
            format!("daily {day}"),
            s.used_day_usd(day),
            amount,
            p.cap_per_day_usd,
        )?;
        Self::check_limit(format!("{role} daily {day}"), day_role, amount, role_day)
    }

    fn take(&mut self, id: u64) -> Result<Reservation, BudgetError> {
        let idx = self
            .state
            .reservations
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| BudgetError::Invalid(format!("unknown reservation {id}")))?;
        Ok(self.state.reservations.remove(idx))
    }
}

pub struct BudgetLedger {
    inner: Mutex<Inner>,
}

pub type SharedLedger = Arc<BudgetLedger>;

impl fmt::Debug for BudgetLedger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BudgetLedger").finish_non_exhaustive()
    }
}

/// A config may only tighten the stored cap and expiry; reservations left
/// pending by a crash stay counted as ambiguous until reconciled.
fn reconcile(mut s: LedgerState, policy: &BudgetPolicy) -> LedgerState {
    s.cap_total_usd = s.cap_total_usd.min(policy.cap_total_usd);
    s.expiry_unix = s.expiry_unix.min(policy.expiry.timestamp());
    for r in &mut s.reservations {
        if r.state == ReservationState::Pending {
            r.state = ReservationState::Ambiguous;
            r.note.get_or_insert_with(|| "unsettled at restart".into());
        }
    }
    s
}

fn iso(unix: i64) -> String {
    DateTime::<Utc>::from_timestamp(unix, 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_else(|| unix.to_string())
}

impl BudgetLedger {
    /// Load (or create) the ledger. A missing file starts a fresh ledger; an
    /// unreadable one is an error, never a reset.
    pub fn open(
        policy: BudgetPolicy,
        mut store: Box<dyn BudgetStore>,
    ) -> Result<Self, BudgetError> {
        let state = Self::load_state(&mut *store, &policy)?;
        let mut inner = Inner {
            state: reconcile(state, &policy),
            policy,
            store,
        };
        inner.persist()?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Read the stored state or start fresh; a corrupt ledger is never a reset.
    fn load_state(
        store: &mut dyn BudgetStore,
        policy: &BudgetPolicy,
    ) -> Result<LedgerState, BudgetError> {
        let loaded = store.load().map_err(|e| match e.kind() {
            io::ErrorKind::InvalidData => BudgetError::Corrupt(e.to_string()),
            _ => BudgetError::Persistence(format!("load: {e}")),
        })?;
        match loaded {
            None => Ok(LedgerState::fresh(policy)),
            Some(s) => {
                Self::check_stored(&s, policy)?;
                Ok(s)
            }
        }
    }

    fn check_stored(s: &LedgerState, policy: &BudgetPolicy) -> Result<(), BudgetError> {
        if s.version != LEDGER_VERSION {
            return Err(BudgetError::Corrupt(format!(
                "unsupported version {}",
                s.version
            )));
        }
        if !(s.cap_total_usd.is_finite() && s.cap_total_usd > 0.0) {
            return Err(BudgetError::Corrupt("stored cap is not positive".into()));
        }
        if policy.cap_total_usd > s.cap_total_usd + EPS {
            return Err(BudgetError::CapRaised {
                stored_usd: s.cap_total_usd,
                requested_usd: policy.cap_total_usd,
            });
        }
        if policy.expiry.timestamp() > s.expiry_unix {
            return Err(BudgetError::ExpiryExtended {
                stored_utc: iso(s.expiry_unix),
                requested_utc: policy.expiry.to_rfc3339(),
            });
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves state consistent (every
        // mutation is followed by persist), so recover the guard.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Atomically check every gate and persist a reservation of `amount_usd`.
    /// `call_end` is the latest instant the call could still be running.
    pub fn reserve(
        &self,
        role: Role,
        model: &str,
        amount_usd: f64,
        now: DateTime<Utc>,
        call_end: DateTime<Utc>,
    ) -> Result<u64, BudgetError> {
        let mut g = self.lock();
        if let Some(reason) = &g.state.halted {
            return Err(BudgetError::Halted(reason.clone()));
        }
        if now.timestamp() >= g.state.expiry_unix || call_end.timestamp() > g.state.expiry_unix {
            return Err(BudgetError::Expired {
                expiry_utc: iso(g.state.expiry_unix),
            });
        }
        if !(amount_usd.is_finite() && amount_usd > 0.0) {
            return Err(BudgetError::Invalid(format!("amount {amount_usd}")));
        }
        let day = day_key(now);
        g.check_affordable(role, &day, amount_usd)?;
        let id = g.state.next_id;
        g.state.next_id += 1;
        g.state.reservations.push(Reservation {
            id,
            role,
            model: model.to_string(),
            day,
            amount_usd,
            created_unix: now.timestamp(),
            state: ReservationState::Pending,
            note: None,
        });
        if let Err(e) = g.persist() {
            g.state.reservations.retain(|r| r.id != id);
            return Err(e);
        }
        Ok(id)
    }

    /// Replace a reservation with its actual charge.
    pub fn settle(&self, id: u64, actual_usd: f64) -> Result<(), BudgetError> {
        let mut g = self.lock();
        let r = g.take(id)?;
        if !(actual_usd.is_finite() && actual_usd >= 0.0) {
            let mut r = r;
            r.state = ReservationState::Ambiguous;
            r.note = Some(format!("invalid charge {actual_usd}"));
            g.state.reservations.push(r);
            g.state
                .halted
                .get_or_insert_with(|| "invalid_charge".into());
            return g.persist();
        }
        if actual_usd > r.amount_usd + EPS {
            g.state
                .halted
                .get_or_insert_with(|| "charge_exceeded_reservation".into());
        }
        g.state.committed_usd.add(r.role, actual_usd);
        g.state
            .committed_by_day
            .entry(r.day.clone())
            .or_default()
            .add(r.role, actual_usd);
        match r.role {
            Role::Decider => g.state.settled_calls.decider += 1,
            Role::Helper => g.state.settled_calls.helper += 1,
        }
        g.persist()
    }

    /// Drop a reservation for a request that was definitely not charged.
    pub fn release(&self, id: u64) -> Result<(), BudgetError> {
        let mut g = self.lock();
        g.take(id)?;
        g.persist()
    }

    /// Keep the reservation counted until reconciled; optionally halt.
    pub fn mark_ambiguous(
        &self,
        id: u64,
        note: &str,
        halt: Option<&str>,
    ) -> Result<(), BudgetError> {
        let mut g = self.lock();
        if let Some(r) = g.state.reservations.iter_mut().find(|r| r.id == id) {
            r.state = ReservationState::Ambiguous;
            r.note = Some(note.to_string());
        }
        if let Some(reason) = halt {
            g.state.halted.get_or_insert_with(|| reason.to_string());
        }
        g.persist()
    }

    /// Stop all new paid calls.
    pub fn halt(&self, reason: &str) {
        let mut g = self.lock();
        g.state.halted.get_or_insert_with(|| reason.to_string());
        let _ = g.persist();
    }

    pub fn halted(&self) -> Option<String> {
        self.lock().state.halted.clone()
    }

    pub fn snapshot(&self) -> LedgerState {
        self.lock().state.clone()
    }

    pub fn policy(&self) -> BudgetPolicy {
        self.lock().policy
    }

    /// Committed plus outstanding for `now`'s UTC day, all roles.
    pub fn spent_today_usd(&self, now: DateTime<Utc>) -> f64 {
        self.lock().state.used_day_usd(&day_key(now))
    }

    /// Cumulative cap minus committed spend and outstanding reservations.
    pub fn remaining_usd(&self) -> f64 {
        let g = self.lock();
        g.state.cap_total_usd - g.state.used_total_usd()
    }
}

#[cfg(test)]
mod tests;
