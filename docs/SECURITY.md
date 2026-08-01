# Security model (research / ghost trading)

**Limen-Capital is an experimental SNN-HFT research stack.** It is not a production
exchange gateway. Default configuration assumes a **single-user, loopback-trust** host.

## What is trusted by default

| Surface | Default | Trust assumption |
|---------|---------|------------------|
| Binary ZMQ MarketPulse / ReadoutPacket | `tcp://127.0.0.1:5555` / `:5556` | Only local processes publish; plain ZMQ (no CURVE) |
| JSON TradeSignal adapter | User-scoped IPC under `$XDG_RUNTIME_DIR/limen-capital/` (or `/tmp/limen-capital-$UID/`) | Same OS UID; override with validated `LIMEN_JSON_IPC` |
| Ghost wallet (metabolic-ledger) | Local JSONL paper ledger | No exchange credentials |
| MarketPulse floats | Validated finite; prices `> 0`; vols clamped to `[0, 1]` | Malformed frames fail closed |

## What this stack does **not** do

- Live capital / mainnet order routing
- Exchange API key management
- Multi-tenant host hardening beyond user-scoped IPC defaults
- ZAP / client public-key allowlisting for CURVE (see open issue on optional ZAP)

## Enabling ZMQ CURVE (optional)

Research default is **plain ZMQ** on loopback.

**Not end-to-end yet:** only the Julia brain path currently has CURVE hooks. The bundled Rust publisher/subscriber peers still use plain ZMQ. Setting `ZMQ_CURVE=1` on the brain alone will break MarketPulse / ReadoutPacket links until both sides use compatible CURVE roles and keys. Treat this as a future dual-side upgrade (tracked with optional ZAP allowlisting).

When both ends support CURVE:

1. Set `ZMQ_CURVE=1` on **every** peer (brain and muscle).
2. Provide real CURVE material via env only (never commit keys):
   - `ZMQ_SERVER_KEY` / `ZMQ_CLIENT_KEY` (and any peer public keys your build expects).
3. Do **not** treat CURVE alone as multi-user authentication until an allowlist (ZAP) lands.

## Safer JSON IPC

Do not use a world-predictable path such as `/tmp/spikenaut_signals.ipc`.

- **Default:** `$XDG_RUNTIME_DIR/limen-capital/signals.ipc` when `XDG_RUNTIME_DIR` is set;
  otherwise `/tmp/limen-capital-$UID/signals.ipc` (numeric OS UID). For defaults only, Capital
  creates the directory and requires mode `0700` (failures abort setup).
- **Override:** `LIMEN_JSON_IPC` must be `ipc://` + absolute filesystem path with no `..` segments
  (e.g. `ipc:///run/user/1000/limen-capital/signals.ipc`). Invalid overrides are rejected.
  **Override parent directories are not auto-created or `chmod`’d** — the operator must secure them.
- Bind collisions: publisher does **not** unlink a live Unix socket. Set `LIMEN_JSON_IPC_REPLACE=1`
  only to clear a known-stale path after a crash.
- Publisher (`strategy/signal_broadcaster.jl`) and consumer (`execution`, `LIMEN_WIRE=json`) must agree.

## Untrusted / multi-user hosts

If other users can run processes on the same machine:

- Prefer binary loopback with host firewall / namespace isolation, or disable unused listeners.
- Always use user-scoped IPC (defaults above) or a private path via `LIMEN_JSON_IPC`.
- Treat any non-loopback bind as out of scope for this research threat model.
- Do not place secrets in the tree; use env vars and ignore rules (`.env`, vault dirs).

## Residual risks (known)

1. **No ZAP allowlist** — CURVE encrypts but does not yet restrict which client keys are accepted.
2. **Loopback is not multi-tenant security** — co-located attackers with the same privileges can still talk to local sockets.
3. **Ghost ledger only** — accidental wiring to a live broker is operator error; this repo does not ship exchange keys.

## Reporting

Private vulnerability reports: use GitHub Security Advisories for this repository —

https://github.com/rmems/Limen-Capital/security/advisories/new

Do not file public issues that include live credentials or private keys.
