# Security model: research stack and live micro-capital

**Limen-Capital is an experimental SNN-HFT research stack.** It is not a production
exchange gateway. The research path assumes a **single-user, loopback-trust** host.
The separate `limen-live` experiment targets real Alpaca capital and requires the
additional controls below. Research isolation is not a live-money safety claim.

## Live launch status and release blockers

On this revision, `limen-live` is behind `live-robinhood`, reads legacy
`ROBINHOOD_API_KEY` / `ROBINHOOD_PRIVATE_KEY`, and refuses `paper = false` in
both the CLI and loop setup. It has no Alpaca adapter or halt-file handler.
Claude setup is also refused. **Do not put real keys on a box for this revision.**

Before any real key is provisioned, the release must include and verify:

- [#41 / RM-2460](https://github.com/rmems/Limen-Capital/issues/41): Alpaca
  place/cancel/poll and removal of Robinhood and Ed25519 signing.
- [#40 / RM-2461](https://github.com/rmems/Limen-Capital/issues/40): allowlisted
  Claude roles and accounted, bounded LLM spend.
- [#42 / RM-2462](https://github.com/rmems/Limen-Capital/issues/42): durable
  reconciled account state, single-instance lock, cancel-on-stop, and halt file.
- [#43 / RM-2463](https://github.com/rmems/Limen-Capital/issues/43): explicit
  live unlock, equity-loss halt, stop-loss/max-hold, spread gate, asset validation.
- [#26 / RM-2446](https://github.com/rmems/Limen-Capital/issues/26):
  protected `main`, working/pinned CI/toolchain, and effective secret detection.
  This is a **release blocker**, not work duplicated by this documentation.

The [runbook](live-runbook.md) is a blocked launch checklist, not permission to
bypass those dependencies. Examples must be reconciled with the submitting
binary before the first real session; RM-2464 is not complete until then.

## Live credentials, host, and audit boundary

- Secrets enter the process through **environment variables only**, injected
  from a private local store or secret manager. Never use CLI arguments, TOML,
  committed `.env` files, journal text, prompts, debug dumps, or screenshots.
  The binary does not load `.env` automatically. `.env.example` contains empty
  slots, not credentials; Alpaca env names must be confirmed against #41.
- Use a **dedicated Alpaca API key pair on a separately funded micro account**,
  never a primary portfolio account. Keep funding minimal, no leverage or
  shorts, one instrument per run, and no other account traders. Application
  caps do not constrain a stolen key or guarantee a maximum realized loss.
- Use a dedicated **Anthropic key with its own workspace spend limit**. Set
  provider limits as well as local daily/experiment limits; do not rely on a
  prompt or estimated token cost to enforce billing. Verify current model
  availability and pricing. Planned roles are `claude-sonnet-5-5` for decisions
  and optional `claude-haiku-5-5` for non-trading helper tags. Current main
  validates only a nonempty model string; this is not an enforced allowlist yet.
- Alpaca authenticates with `APCA-API-KEY-ID` / `APCA-API-SECRET-KEY` **HTTP
  headers**, not signed requests. Use the official live HTTPS origin
  `https://api.alpaca.markets` and the adapter's official data endpoint. Never
  send real credentials to a mock, proxy, or arbitrary base-URL override.
- Keep a sane, synchronized system clock (for example, NTP). UTC daily
  accounting, session timestamps, quote age, and market-hours logic depend on
  it even though Alpaca requests are not signed. Check the venue clock/calendar,
  including holidays, early closes, and US Eastern daylight-saving changes.
- HTTP mocks are **loopback-only**, with dummy credentials, no remote/wildcard
  listeners and no remote plain HTTP. Mocks exercise failure paths; they are
  not a paper phase or shadow soak and cannot pass the live-soak gate.
- Use a dedicated OS user, `umask 077`, a private working directory and private
  journal/state/ledger files. Keep all audit artifacts out of git and training
  ingestion. Journal context goes to the decider: treat quote/LLM/journal text as
  untrusted input, never as authority to change caps, credentials, or orders.
- Preserve durable state across stops; do not clear a loss/spend baseline or
  lock to restart trading. Cancellation is not liquidation. A stopped process
  can leave exposure, and an order can fill while cancellation is pending.
  Verify orders and holdings directly in Alpaca before considering a halt safe.
- If a key leaks, stop the session, revoke and rotate the affected keys
  immediately, then cancel/verify outstanding orders through the venue and
  inspect fills/billing. Do not paste credentials or raw account responses
  into an incident issue.

## What is trusted by default

| Surface | Default | Trust assumption |
|---------|---------|------------------|
| Binary ZMQ MarketPulse / ReadoutPacket | `tcp://127.0.0.1:5555` / `:5556` | Only local processes publish; plain ZMQ (no CURVE) |
| JSON TradeSignal adapter | User-scoped IPC under `$XDG_RUNTIME_DIR/limen-capital/` (or `/tmp/limen-capital-$UID/`) | Same OS UID; override with validated `LIMEN_JSON_IPC` |
| Ghost wallet (metabolic-ledger) | Local JSONL paper ledger | No exchange credentials |
| MarketPulse floats | Finite required; prices `> 0` rejected otherwise; vols **clamped** to `[0, 1]` (accepted after clamp, not fail-closed) | Non-finite / non-positive price fail closed |

## What the research path does **not** do

- Live capital / mainnet order routing (the separate `limen-live` path targets it)
- Exchange API key management (live secrets are operator-provisioned via env)
- Multi-tenant host hardening beyond user-scoped IPC defaults
- ZAP allowlist is optional when `ZMQ_CURVE=1` (see below); default stack does not use CURVE

## Enabling ZMQ CURVE (optional)

Research default is **plain ZMQ** on loopback.

Binary bind/connect endpoints are checked in the brain, muscle, and market
simulator. Non-loopback addresses, hostnames, and wildcard binds require
`LIMEN_ALLOW_REMOTE_WIRE=1`. This opt-in does not enable encryption or
authentication: use CURVE and an allowlist on untrusted networks.
The required v2 magic/version header prevents accidental schema mismatches,
not forgery. Timestamp watermarks reject duplicate, stale, future, and pre-start
frames; they require synchronized clocks and are not cryptographic anti-replay.

When `ZMQ_CURVE=1`, brain (`spike_helm.jl`), muscle (`execution`), and `market_sim` configure CURVE on the binary wire. The default without `ZMQ_CURVE=1` is unchanged plain TCP loopback.

**CURVE roles (fixes one-sided mis-bindings — see issue #24):**

| Peer | Socket | Role |
|------|--------|------|
| `market_sim` | PUB `:5555` | CURVE **server** |
| brain | SUB → `:5555` | CURVE **client** (to market) |
| brain | PUB `:5556` | CURVE **server** |
| muscle | SUB ×2 | CURVE **client** (separate sockets per remote server public key) |

Keys via env only (never commit):

- Server bind secrets: `ZMQ_MARKET_SERVER_SECRET` / `ZMQ_READOUT_SERVER_SECRET` (or legacy `ZMQ_SERVER_KEY` per role)
- Client identity: `ZMQ_CLIENT_SECRET_KEY` + `ZMQ_CLIENT_PUBLIC_KEY` (or legacy `ZMQ_SERVER_KEY` for single-key research setups on brain)
- Remote server public keys: `ZMQ_MARKET_SERVER_PUBLIC_KEY`, `ZMQ_READOUT_SERVER_PUBLIC_KEY` (legacy muscle alias: `ZMQ_CLIENT_KEY` for readout)

### Optional ZAP client allowlist

When `ZMQ_CURVE=1` **and** an allowlist is configured, CURVE **servers** start an in-process ZAP handler and reject handshakes from CURVE clients whose **public key (Z85)** is not listed. Without an allowlist, CURVE still encrypts but accepts any client that completes a valid handshake (same as before).

| Variable | Meaning |
|----------|---------|
| `ZMQ_CURVE_ALLOWLIST` | Comma/whitespace-separated client **public** keys (Z85) |
| `ZMQ_CURVE_ALLOWLIST_FILE` | Path to one public key per line (`#` comments allowed) |
| `ZMQ_ZAP_DOMAIN` | ZAP domain on server sockets (default `limen-capital`) |

Generate keypairs at runtime (example): `cargo run -p limen-execution --example version` is not a keygen — use `zmq_curve_keypair` from libzmq tooling or the unit tests’ `CurveKeyPair::new()` pattern in CI-only fixtures.

## Safer JSON IPC

Do not use a world-predictable path such as `/tmp/spikenaut_signals.ipc`.

- **Default:** `$XDG_RUNTIME_DIR/limen-capital/signals.ipc` when `XDG_RUNTIME_DIR` is set;
  otherwise `/tmp/limen-capital-$UID/signals.ipc` (numeric OS UID). For defaults only, Capital
  creates the directory and requires mode `0700` (failures abort setup).
- **Override:** `LIMEN_JSON_IPC` must be `ipc://` + absolute filesystem path with no `..` segments
  (e.g. `ipc:///run/user/1000/limen-capital/signals.ipc`). Invalid overrides are rejected.
  An **empty** override is treated as unset (falls back to defaults).
  **Same-UID isolation applies to default endpoints**; override parents are **not** auto-created
  or `chmod`’d — the operator must secure them.
- **IPC ownership:** before `ipc://` bind, the publisher holds `flock(LOCK_EX|LOCK_NB)` on
  `*.owner.lock` for the process lifetime. This counters libzmq’s unlink-on-bind steal of a
  live socket among **cooperative Capital publishers**. The lock file is left on disk (not
  deleted on shutdown) so ownership stays tied to a stable inode; the flock ends when the
  process exits or `shutdown` closes the fd. Non-flock binders on the same path are outside
  this protocol and are not protected.
- Publisher (`strategy/signal_broadcaster.jl`) and consumer (`execution`, `LIMEN_WIRE=json`) must agree.

## Untrusted / multi-user hosts

If other users can run processes on the same machine:

- Prefer binary loopback with host firewall / namespace isolation, or disable unused listeners.
- Always use user-scoped IPC (defaults above) or a private path via `LIMEN_JSON_IPC`.
- Treat any non-loopback bind as out of scope for this research threat model.
- Do not place secrets in the tree; use env vars and ignore rules (`.env`, vault dirs).

## Residual risks (known)

1. **ZAP is opt-in** — without `ZMQ_CURVE_ALLOWLIST*` set, CURVE does not restrict client keys.
2. **Loopback is not multi-tenant security** — co-located attackers with the same privileges can still talk to local sockets.
3. **Live losses and costs** — the separate live experiment can lose real money
   once unlocked. Risk caps are gates, not a broker guarantee against gaps,
   stale data, delayed fills, compromised credentials, or unavailable cancels.

## CI and repository controls

CI runs Rust, Julia CPU, integration, and Gitleaks checks on pull requests and
pushes to `main`. Gitleaks scans commit changes across files, including secrets
introduced and removed within a PR. It uses the upstream default rules, not a
single retired CURVE-key match. Comments and artifact uploads are disabled; the
job has read-only permissions. Scanning cannot prove that arbitrary key material
is absent. Keep CURVE secrets in environment variables even if a scanner passes.

For a local history scan, install Gitleaks **8.30.1**, then run from the repo root:

```bash
gitleaks git --redact --log-opts="--all"
```

`execution/rust-toolchain.toml` pins Rust **1.99.0**, matching both CI toolchain
steps. Update all three together when changing the compiler.

The desired `main` ruleset is checked in at `.github/main-ruleset.json`. A file
alone does **not** activate GitHub protection. A repository administrator must
apply it after the four job names above are available on the PR:

```bash
gh api --method POST repos/rmems/Limen-Capital/rulesets \
  --input .github/main-ruleset.json
gh api repos/rmems/Limen-Capital/rulesets
```

If a ruleset with that name already exists, update its ID with `--method PUT`
and `/rulesets/ID` instead of creating a duplicate. Verify the active ruleset
requires a PR and all four checks, with no bypass actors. The configuration also
blocks branch deletion and force pushes and requires an up-to-date PR branch.
Activation requires repository administration permissions; read/push access
alone is insufficient.

## Reporting

Private vulnerability reports: use GitHub Security Advisories for this repository —

https://github.com/rmems/Limen-Capital/security/advisories/new

Do not file public issues that include live credentials or private keys.
