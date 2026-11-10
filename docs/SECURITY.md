# Security model (research / ghost trading)

**Limen-Capital is an experimental SNN-HFT research stack.** It is not a production
exchange gateway. Default configuration assumes a **single-user, loopback-trust** host.

## What is trusted by default

| Surface | Default | Trust assumption |
|---------|---------|------------------|
| Binary ZMQ MarketPulse / ReadoutPacket | `tcp://127.0.0.1:5555` / `:5556` | Only local processes publish; plain ZMQ (no CURVE) |
| JSON TradeSignal adapter | User-scoped IPC under `$XDG_RUNTIME_DIR/limen-capital/` (or `/tmp/limen-capital-$UID/`) | Same OS UID; override with validated `LIMEN_JSON_IPC` |
| Ghost wallet (metabolic-ledger) | Local JSONL paper ledger | No exchange credentials |
| MarketPulse floats | Finite required; prices `> 0` rejected otherwise; vols **clamped** to `[0, 1]` (accepted after clamp, not fail-closed) | Non-finite / non-positive price fail closed |

## What this stack does **not** do

- Live capital / mainnet order routing
- Exchange API key management
- Multi-tenant host hardening beyond user-scoped IPC defaults
- ZAP allowlist is optional when `ZMQ_CURVE=1` (see below); default stack does not use CURVE

## Enabling ZMQ CURVE (optional)

Research default is **plain ZMQ** on loopback.

Binary bind/connect endpoints are checked in the brain, muscle, and market
simulator. Non-loopback addresses, hostnames, and wildcard binds require
`LIMEN_ALLOW_REMOTE_WIRE=1`. This opt-in does not enable encryption or
authentication: use CURVE and an allowlist on untrusted networks.
The required topic/version envelope prevents accidental schema mismatches,
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

Generate keypairs at runtime (example): `cargo run -p spikenaut-execution-engine --example version` is not a keygen — use `zmq_curve_keypair` from libzmq tooling or the unit tests’ `CurveKeyPair::new()` pattern in CI-only fixtures.

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
3. **Ghost ledger only** — accidental wiring to a live broker is operator error; this repo does not ship exchange keys.

## Reporting

Private vulnerability reports: use GitHub Security Advisories for this repository —

https://github.com/rmems/Limen-Capital/security/advisories/new

Do not file public issues that include live credentials or private keys.
