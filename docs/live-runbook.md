# limen-live: first supervised live session (RM-2464)

**Status: BLOCKED, not launch-ready.** The checked-in binary still uses the
legacy Robinhood adapter and rejects `paper = false`. No Alpaca live-quotes
preflight, real-order session, or halt-file command is available on this
revision. Do not provision real keys. Do not bypass the refusals.

Raul's 2026-11-10 direction: **no paper trading, no paper phase, no shadow
soak**. The first live session is the soak: supervised real orders at micro
caps. Local dummy-credential mock tests remain required software tests, not a
replacement soak. This document defines the acceptance checklist in advance;
it does not record a passed session.

## 1. Release gate, before real keys

- [ ] [#26 / RM-2446](https://github.com/rmems/Limen-Capital/issues/26) is
  resolved and its branch protection, CI pins/toolchain, and secret checks are
  verified. It is a release blocker for this binary, not part of this docs fix.
- [ ] [#41 / RM-2460](https://github.com/rmems/Limen-Capital/issues/41) ships
  Alpaca place/cancel/poll; Robinhood and its signing code are removed.
- [ ] [#40 / RM-2461](https://github.com/rmems/Limen-Capital/issues/40) ships
  Sonnet decisions, optional Haiku helper tags, enforced role/model allowlists,
  parse-error reporting, and bounded, durable LLM billing.
- [ ] [#42 / RM-2462](https://github.com/rmems/Limen-Capital/issues/42) ships
  venue reconciliation, durable loss/spend/fill/order state, a single-instance
  lock, halt file and cancellation on halt/exit.
- [ ] [#43 / RM-2463](https://github.com/rmems/Limen-Capital/issues/43) unlocks
  live submission and ships equity-loss, stop-loss/max-hold, spread and asset
  gates. Mock tests prove risk rejects never submit and restart never duplicates.
  Verify the session's order ceiling on exits too: current legacy
  `risk::check` applies `max_order_usd` to buys only, so it does not yet prove
  the all-order soak ceiling. Resolve this with the live implementation before
  launch; do not silently exempt sells or relax the session requirement.
- [ ] Update this runbook, `config/live.example.toml`, and `.env.example` to
  the **merged binary's exact** feature name, source/schema, credential names,
  halt path and journal events. Test its real CLI entry point with dummy
  credentials. The draft config deliberately fails current symbol validation.
- [ ] Record the release commit, successful CI checks and operator approval.
  A merged docs PR alone does not clear this gate or complete RM-2464.

## 2. Write the session plan before starting

Keep this record privately with the session journal, not in a public issue.
Fill every blank; no session begins with an unspecified kill criterion.

| Item | Operator records |
| --- | --- |
| Release commit / config checksum | |
| Supervisor and backup contact | |
| One symbol / venue asset validation | |
| UTC start, planned end, maximum ticks | planned tick count must be a positive integer (zero is unbounded) |
| Venue open/close, including early close | |
| Separately funded micro account confirmed (no keys/account IDs here) | |
| Position cap, including resting exposure | at most $10 |
| Every order cap, buy **and** sell | prefer $1; at most $5 |
| Daily equity-loss cap (realized + unrealized) | at most $4 |
| Daily / whole-experiment LLM caps, provider workspace limit | |
| Stop-loss bps / maximum hold ticks / maximum spread bps | |
| Quote-age and clock-offset limits | |
| Parse-error kill threshold / observation window | |
| Halt-file absolute path / expected detection deadline | |
| Cancel confirmation deadline / venue fallback operator | |
| End-of-session residual-position plan | |

Use a dedicated OS user and private directory with `umask 077`. Follow
[SECURITY](SECURITY.md#live-credentials-host-and-audit-boundary) before injecting
environment secrets. Use one process/account, no parallel manual traders,
auto-restarts, or unrelated holdings. Keep durable state and a fresh journal
for this session; never reset daily accounting to get another allowance.

The planned decision model is `claude-sonnet-5-5`; optional `claude-haiku-5-5`
helpers only label the journal and cannot trade. Confirm availability, pricing,
credit expiry and enforced role allowlists in the released implementation.
No aliases, unpriced model substitutions, or prompt-based budget enforcement.

## 3. Read-only live-quote preflight

**Unavailable until section 1 passes.** On the released Alpaca binary, run
`limen-live --config <approved-config> --once --journal <private-preflight-journal>`
using its actual built executable path. Angle-bracket values are placeholders,
not shell commands to paste. Retain explicit `paper = false`; do not pass
`--paper`. `--once` must independently suppress all order writes.

Require a successful single tick showing the live venue account summary,
configured asset rules, fresh live bid/ask, decision and risk verdict, with
**zero submitted orders**. Check Alpaca directly for zero new orders. An
error exit, stale quote, closed session, or missing decision row is not a
passed preflight. Read-only here means no venue writes; a decider call can
still cost money and must be accounted. This is a check, not a shadow soak.

Current-main limitations are not an alternative: `--once` with the legacy
adapter is not an Alpaca preflight; `--paper --once` uses a simulated book.

## 4. First live session, the soak

Only after sections 1–3 pass, run the released executable with
`--config <approved-config> --max-ticks <planned-count> --journal <private-session-journal>`,
without `--once` or `--paper`, and supervise it continuously.
The approved config must explicitly set `paper = false`. `<planned-count>`
must be a positive integer: a value of `0` is the unbounded default and a
missing flag leaves the session with no tick limit, so neither is acceptable.

- Confirm the journal header says **LIVE**, Alpaca, the chosen symbol and
  approved models/caps. Watch the first real order through acceptance and
  terminal status. A proposal or accepted order is not a fill.
- Use fractional notional sizing on a venue-validated active, tradable,
  fractionable US stock/ETF. Alpaca supports $1 fractional orders; do not
  raise caps to buy a whole share. Limit orders only, one size field (`qty`
  or `notional`, not both), buy at bid/sell at ask, `time_in_force = day`,
  no shorts, leverage, or extended-hours opt-in.
- For this experiment orders fill **only in regular market hours**, normally
  09:30–16:00 America/New_York. Check Alpaca's clock/calendar for holidays and
  early closes rather than hard-coding a UTC window. Unfilled day orders
  expire/are canceled at the close; verify terminal status by polling.
  Orders submitted after close can queue for the next trading day, so do not
  submit outside the supervised session or assume a closed market is safe.
- Observe partial fills and pending cancels until terminal. An expired order
  is not a fill; record any earlier partial fills. A lost POST response
  requires lookup by the same `client_order_id`, never a blind retry/new ID.
- Repeat this per-symbol gate before live use of another instrument.

Venue references: [fractional trading](https://docs.alpaca.markets/docs/fractional-trading)
and [order lifecycle / time in force](https://docs.alpaca.markets/docs/orders-at-alpaca).
Alpaca supports broader sessions; this experiment deliberately excludes them.

## 5. Kill criteria and halt procedure

Halt immediately on any risk-gate bypass, cap breach, duplicate/unexplained
order or fill, loss threshold, unknown/reconciliation-inconsistent holdings,
lost journal/durable-state writes, secret exposure, lost supervision, or
unconfirmed cancel. Also halt at the planned end or any prewritten
parse-error, spread, quote-age, clock, stop-loss/max-hold, or LLM spend limit.
Do not raise caps or change kill criteria mid-session.

The halt file belongs to #42. **Current main never checks a halt file**;
touching an arbitrary file does nothing. Before release, replace the
placeholder below with the implemented path and tested detection deadline:

1. Create the configured halt file (`touch <absolute-halt-path>`), or use
   Ctrl-C if the released cancellation path has been verified. Stop new
   decisions/submissions; retain the halt file and durable state.
2. Verify every resting order is canceled or otherwise terminal in Alpaca,
   including any fill racing the cancel. Cancellation requests and
   `pending_cancel` do not prove cancellation. If the process or venue API is
   unresponsive, stop the process and use the authenticated Alpaca dashboard
   to cancel and inspect orders. Do not wait for a journal to recover first.
3. Reconcile remaining holdings and fills. **Halt is not liquidation**.
   Follow the prewritten residual-position plan under supervision; do not
   assume a limit exit will fill or a loss cap guarantees a $4 maximum loss.
4. Preserve journal/state/ledger and provider usage. No automatic restart,
   removal of the halt file, state deletion, or same-day loss-counter reset.
   Investigate and obtain operator approval before resuming.

### What a halt looks like in the journal

Current main can only demonstrate the legacy paper daily-loss row:
`action = none`, `reason = daily loss $… ≥ cap $…`,
`risk = rejected:daily_loss_breached`; stdout ends with
`halted=daily_loss_breached`. LLM exhaustion is
`rejected:llm_budget_exhausted` and **idles**, rather than halting.
Ctrl-C appears in the run summary as `halted=ctrl_c`, not a dedicated journal
halt row. There is **no current halt-file journal event**, equity-loss halt,
live cancellation confirmation, or durable daily-loss baseline.

Before release, replace this paragraph with captured mock examples from #42–#43
showing the exact halt-file and equity-loss journal tags, timestamps, stop
reason, cancel requests, terminal statuses/partial fills and residual holdings.
Require no subsequent submission after halt detection. A missing tail row or
process exit is not proof of a safe halt. Missing halt/cancel evidence blocks
launch and fails the soak; never fabricate an expected event name.

## 6. Post-session acceptance, from the journal

The supervisor records the following privately, referencing journal rows and
reconciling them against venue fills and provider usage:

- [ ] At least one actual real-money order and fill observed. A session with
  no real execution is **inconclusive**, not a passed soak.
- [ ] **Zero risk-gate violations**: every submitted order has a preceding
  approved verdict, allowed limit/day payload, correct size and exposure,
  and no submission follows a rejection or detected halt. Count ordinary
  correctly blocked proposals separately; a rejection is not a violation.
  Check worst-case exposure including resting/partially filled orders, all
  order sizes (including exits), and daily realized plus unrealized loss.
- [ ] Record parse errors / total decision-response parse attempts, including
  retries: `errors / attempts = rate`. Separate transport errors and helper
  parse attempts. If attempts are zero, report `N/A`, not 0%. Reference
  journal evidence; inability to identify attempts/errors is a failed gate.
  Main currently records no such LLM attempts and cannot pass this check.
- [ ] Sum journal `llm $` across all calls/roles/retries, including failures and
  cache charges, for both the UTC day (including preflight/earlier sessions)
  and whole experiment. Record totals and configured caps. Both must be
  inside the caps and agree with durable billing/provider usage. Rounded
  display values alone cannot certify a near-cap total; require sufficient
  journal precision. Missing or unaccounted cost is a failed gate.
- [ ] Orders are terminal and final positions/cash/realized PnL agree with
  Alpaca and the ledger. Document residual exposure and its supervised plan.
- [ ] Record halt reason and cancellation evidence, UTC boundaries, config
  checksum, release commit, violations count, parse rate, spend/caps and
  operator pass/fail/inconclusive decision. Preserve the original journal.

Profit is not a pass criterion. A journal is **not a research result** and
neither it nor LLM output ever enters a training set, research metrics,
walk-forward evaluation, or a calibration corpus. Do not commit journals,
account responses, durable state, or billing artifacts.
