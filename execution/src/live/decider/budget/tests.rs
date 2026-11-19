use super::*;
use chrono::TimeZone;
use std::sync::atomic::{AtomicBool, Ordering};

fn t(d: u32, h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap()
}

fn expiry() -> DateTime<Utc> {
    // 2026-11-12T23:59:59-08:00
    Utc.with_ymd_and_hms(2026, 11, 13, 7, 59, 59).unwrap()
}

fn policy(total: f64, day: f64) -> BudgetPolicy {
    BudgetPolicy {
        cap_total_usd: total,
        cap_per_day_usd: day,
        helper_total_usd: 0.0,
        helper_per_day_usd: 0.0,
        expiry: expiry(),
    }
}

/// In-memory store whose writes can be made to fail.
#[derive(Clone, Default)]
struct MemStore {
    state: Arc<Mutex<Option<LedgerState>>>,
    fail: Arc<AtomicBool>,
}

impl BudgetStore for MemStore {
    fn load(&mut self) -> io::Result<Option<LedgerState>> {
        Ok(self.state.lock().unwrap().clone())
    }
    fn save(&mut self, s: &LedgerState) -> io::Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(io::Error::other("disk full"));
        }
        *self.state.lock().unwrap() = Some(s.clone());
        Ok(())
    }
}

fn open(p: BudgetPolicy, store: &MemStore) -> BudgetLedger {
    BudgetLedger::open(p, Box::new(store.clone())).unwrap()
}

/// Test shorthand for `BudgetLedger::reserve` where `now == call_end`.
fn reserve(l: &BudgetLedger, role: Role, usd: f64, t: DateTime<Utc>) -> Result<u64, BudgetError> {
    l.reserve(&ReserveRequest {
        role,
        model: "m",
        amount_usd: usd,
        now: t,
        call_end: t,
    })
}

fn spend(l: &BudgetLedger, role: Role, usd: f64, now: DateTime<Utc>) -> Result<(), BudgetError> {
    let id = reserve(l, role, usd, now)?;
    l.settle(id, usd)
}

#[test]
fn exact_cumulative_boundary_then_reject_before_dispatch() {
    let store = MemStore::default();
    let l = open(policy(1.0, 0.75), &store);
    spend(&l, Role::Decider, 0.5, t(10, 1)).unwrap();
    spend(&l, Role::Decider, 0.25, t(10, 2)).unwrap();
    // Day cap hit exactly; tomorrow brings the total exactly to 1.0.
    assert!(matches!(
        reserve(&l, Role::Decider, 0.01, t(10, 3)),
        Err(BudgetError::Exhausted { .. })
    ));
    spend(&l, Role::Decider, 0.25, t(11, 1)).unwrap();
    assert!(l.remaining_usd().abs() < 1e-12);
    let err = reserve(&l, Role::Decider, 0.000001, t(11, 2)).unwrap_err();
    match err {
        BudgetError::Exhausted { scope, .. } => assert_eq!(scope, "cumulative"),
        e => panic!("{e:?}"),
    }
    // The cap was not raised and nothing new is outstanding.
    let s = l.snapshot();
    assert_eq!(s.cap_total_usd, 1.0);
    assert!(s.reservations.is_empty());
    assert_eq!(s.settled_calls.decider, 3);
}

#[test]
fn outstanding_reservations_count_against_remaining() {
    let l = open(policy(1.0, 0.9), &MemStore::default());
    let a = reserve(&l, Role::Decider, 0.6, t(10, 1)).unwrap();
    assert!((l.remaining_usd() - 0.4).abs() < 1e-12);
    assert!(reserve(&l, Role::Decider, 0.5, t(10, 1)).is_err());
    // Settling below the reservation frees the difference.
    l.settle(a, 0.1).unwrap();
    assert!((l.remaining_usd() - 0.9).abs() < 1e-12);
    reserve(&l, Role::Decider, 0.5, t(10, 1)).unwrap();
}

#[test]
fn simultaneous_reservations_never_overbook() {
    let l = Arc::new(open(policy(2.0, 1.0), &MemStore::default()));
    let handles: Vec<_> = (0..64)
        .map(|_| {
            let l = l.clone();
            std::thread::spawn(move || reserve(&l, Role::Decider, 0.1, t(10, 1)).is_ok())
        })
        .collect();
    let granted = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|ok| *ok)
        .count();
    assert_eq!(granted, 10, "day cap $1.00 / $0.10 reservations");
    assert!(l.snapshot().used_day_usd("2026-11-10") <= 1.0 + 1e-9);
}

#[test]
fn day_rollover_resets_only_pacing_never_the_cumulative_total() {
    let l = open(policy(2.5, 1.0), &MemStore::default());
    for day in [10, 11] {
        spend(&l, Role::Decider, 1.0, t(day, 1)).unwrap();
        assert!(reserve(&l, Role::Decider, 0.01, t(day, 2)).is_err());
    }
    // Day 12 gets a fresh pacing window but only $0.50 of cumulative room.
    let err = reserve(&l, Role::Decider, 1.0, t(12, 1)).unwrap_err();
    assert!(
        matches!(&err, BudgetError::Exhausted { scope, .. } if scope == "cumulative"),
        "{err}"
    );
    spend(&l, Role::Decider, 0.5, t(12, 1)).unwrap();
    assert!(l.remaining_usd().abs() < 1e-12);
}

#[test]
fn helper_share_is_carved_out_of_the_decider_allocation() {
    let mut p = policy(2.0, 1.0);
    p.helper_total_usd = 0.3;
    p.helper_per_day_usd = 0.1;
    let l = open(p, &MemStore::default());
    spend(&l, Role::Helper, 0.1, t(10, 1)).unwrap();
    assert!(
        reserve(&l, Role::Helper, 0.01, t(10, 1)).is_err(),
        "helper daily share"
    );
    // The decider still has its full daily share (1.0 - 0.1).
    spend(&l, Role::Decider, 0.9, t(10, 2)).unwrap();
    spend(&l, Role::Helper, 0.1, t(11, 1)).unwrap();
    spend(&l, Role::Helper, 0.1, t(12, 1)).unwrap();
    let err = reserve(&l, Role::Helper, 0.01, t(13, 1)).unwrap_err();
    assert!(
        matches!(&err, BudgetError::Exhausted { scope, .. } if scope == "helper cumulative"),
        "{err}"
    );
    let s = l.snapshot();
    assert!((s.committed_usd.helper - 0.3).abs() < 1e-12);
    assert!((s.committed_usd.decider - 0.9).abs() < 1e-12);
    assert!((s.committed_usd.total() - 1.2).abs() < 1e-12);
}

#[test]
fn no_paid_calls_at_or_after_expiry_or_running_past_it() {
    let l = open(policy(10.0, 5.0), &MemStore::default());
    let e = expiry();
    assert!(matches!(
        reserve(&l, Role::Decider, 0.1, e),
        Err(BudgetError::Expired { .. })
    ));
    let before = e - chrono::Duration::seconds(5);
    // Full-precision expiry: a call that could still run at the cutoff is
    // refused even though `now` is before it; one ending well inside is fine.
    let req = |end_offset: i64| ReserveRequest {
        role: Role::Decider,
        model: "m",
        amount_usd: 0.1,
        now: before,
        call_end: before + chrono::Duration::seconds(end_offset),
    };
    assert!(matches!(
        l.reserve(&req(30)),
        Err(BudgetError::Expired { .. })
    ));
    l.reserve(&req(2)).unwrap();
    assert!(matches!(
        reserve(&l, Role::Decider, 0.1, t(14, 0)),
        Err(BudgetError::Expired { .. })
    ));
}

#[test]
fn restart_keeps_spend_reservations_and_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.json");
    let pending;
    {
        let l = BudgetLedger::open(policy(1.0, 0.9), Box::new(FileStore::open(&path).unwrap()))
            .unwrap();
        spend(&l, Role::Decider, 0.3, t(10, 1)).unwrap();
        pending = reserve(&l, Role::Decider, 0.2, t(10, 2)).unwrap();
        // Process "crashes" here with the reservation in flight.
    }
    let l =
        BudgetLedger::open(policy(1.0, 0.9), Box::new(FileStore::open(&path).unwrap())).unwrap();
    let s = l.snapshot();
    assert!((s.committed_usd.decider - 0.3).abs() < 1e-12);
    assert_eq!(s.reservations.len(), 1);
    assert_eq!(s.reservations[0].id, pending);
    assert_eq!(s.reservations[0].state, ReservationState::Ambiguous);
    assert!((l.remaining_usd() - 0.5).abs() < 1e-12);
    // A new day after restart still sees the cumulative spend.
    assert!(reserve(&l, Role::Decider, 0.6, t(11, 1)).is_err());
    drop(l);
    // Restart after expiry with credits remaining: still no paid calls.
    let l =
        BudgetLedger::open(policy(1.0, 0.9), Box::new(FileStore::open(&path).unwrap())).unwrap();
    assert!(matches!(
        reserve(&l, Role::Decider, 0.01, t(13, 9)),
        Err(BudgetError::Expired { .. })
    ));
}

#[test]
fn config_cannot_raise_the_cap_or_extend_the_deadline_on_restart() {
    let store = MemStore::default();
    drop(open(policy(5.0, 1.0), &store));
    let raised = BudgetLedger::open(policy(200.0, 1.0), Box::new(store.clone())).unwrap_err();
    assert!(matches!(raised, BudgetError::CapRaised { .. }), "{raised}");
    let mut later = policy(5.0, 1.0);
    later.expiry = expiry() + chrono::Duration::hours(1);
    let extended = BudgetLedger::open(later, Box::new(store.clone())).unwrap_err();
    assert!(
        matches!(extended, BudgetError::ExpiryExtended { .. }),
        "{extended}"
    );
    // Lowering is allowed and sticks.
    drop(open(policy(4.0, 1.0), &store));
    let again = BudgetLedger::open(policy(5.0, 1.0), Box::new(store.clone())).unwrap_err();
    assert!(matches!(again, BudgetError::CapRaised { stored_usd, .. } if stored_usd == 4.0));
}

#[test]
fn interrupted_or_corrupt_writes_fail_closed_without_reset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.json");
    {
        let l = BudgetLedger::open(policy(1.0, 0.9), Box::new(FileStore::open(&path).unwrap()))
            .unwrap();
        spend(&l, Role::Decider, 0.4, t(10, 1)).unwrap();
    }
    // A crash mid-write leaves only a stray temp file; the ledger is intact.
    std::fs::write(sibling(&path, ".tmp"), b"{\"version\":1,\"cap").unwrap();
    {
        let l = BudgetLedger::open(policy(1.0, 0.9), Box::new(FileStore::open(&path).unwrap()))
            .unwrap();
        assert!((l.snapshot().committed_usd.decider - 0.4).abs() < 1e-12);
    }
    // A truncated ledger is refused rather than treated as a fresh budget.
    std::fs::write(&path, b"{\"version\":1,\"cap_total").unwrap();
    let err = BudgetLedger::open(policy(1.0, 0.9), Box::new(FileStore::open(&path).unwrap()))
        .unwrap_err();
    assert!(matches!(err, BudgetError::Corrupt(_)), "{err}");
}

#[test]
fn file_store_is_exclusive_across_handles() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.json");
    let first = FileStore::open(&path).unwrap();
    let err = FileStore::open(&path).unwrap_err();
    assert!(err.to_string().contains("in use"), "{err}");
    drop(first);
    FileStore::open(&path).unwrap();
}

#[test]
fn persistence_failure_halts_and_never_grants() {
    let store = MemStore::default();
    let l = open(policy(1.0, 0.9), &store);
    store.fail.store(true, Ordering::SeqCst);
    let err = reserve(&l, Role::Decider, 0.1, t(10, 1)).unwrap_err();
    assert!(matches!(err, BudgetError::Persistence(_)), "{err}");
    assert!(l.snapshot().reservations.is_empty());
    store.fail.store(false, Ordering::SeqCst);
    assert!(matches!(
        reserve(&l, Role::Decider, 0.1, t(10, 1)),
        Err(BudgetError::Halted(r)) if r == "persistence_failed"
    ));
}

#[test]
fn ambiguous_outcomes_stay_reserved_and_overcharges_halt() {
    let l = open(policy(1.0, 0.9), &MemStore::default());
    let a = reserve(&l, Role::Decider, 0.3, t(10, 1)).unwrap();
    l.mark_ambiguous(a, "timeout", None).unwrap();
    assert!((l.remaining_usd() - 0.7).abs() < 1e-12);
    assert!(l.halted().is_none());
    let b = reserve(&l, Role::Decider, 0.1, t(10, 1)).unwrap();
    l.settle(b, 0.2).unwrap();
    assert_eq!(l.halted().as_deref(), Some("charge_exceeded_reservation"));
    assert!(matches!(
        reserve(&l, Role::Decider, 0.01, t(10, 2)),
        Err(BudgetError::Halted(_))
    ));
    // The actual (higher) charge was recorded, not the reservation.
    assert!((l.snapshot().committed_usd.decider - 0.2).abs() < 1e-12);
}
