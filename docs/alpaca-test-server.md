# Shared Alpaca test server (RM-2454)

`execution/tests/support/alpaca.rs` owns the loopback stand-in. Integration suites
import it with `mod support;` and use `support::alpaca::MockAlpaca`. Each test starts
its own instance on `127.0.0.1:0`; "shared" means one implementation, not global
state. Dropping it aborts the listener and active connections. No environment keys,
external venue, account, or real orders are used.

Run its HTTP contract checks:

```sh
cd execution
cargo test --locked --features live-alpaca --test alpaca_mock
```

Point the client base URL at `server.base_url()` and supply the synthetic `KEY_ID`
and `SECRET_KEY` constants as `APCA-API-KEY-ID` and `APCA-API-SECRET-KEY` headers.
Both are checked on every route. `requests()` records method, raw path including
query, headers, and body, including failed/auth-rejected attempts.

## Contract

- `GET /v2/account`: account object, not a paginated envelope.
- `GET /v2/positions`: array, initially empty.
- `POST /v2/orders`: day limit order, symbol, buy/sell, positive limit price,
  nonempty unique `client_order_id`, exactly one positive `qty` or `notional`.
  Responds 200 with an Alpaca-shaped order and decimal strings.
- `GET /v2/orders`: array; default open orders. Supports `status=open|closed|all`,
  comma-separated `symbols`, `side`, `limit`, and `direction`. Unsupported filter
  keys fail rather than silently giving incorrect reconciliation results.
- `GET /v2/orders/{id}` and
  `GET /v2/orders:by_client_order_id?client_order_id=...`: the same stored order.
  Query values are URL-decoded. Missing orders return 404.
- `DELETE /v2/orders/{id}`: empty 204, with immediate cancellation in this mock.
  Filled/canceled/expired/rejected/replaced orders cannot be canceled (422).
- Errors use Alpaca's `code`/`message` object. Unknown routes return 404.

`set_account`, `set_positions`, and `seed_order` provide explicit venue snapshots
for restart, reconciliation, and equity-halt tests. Snapshots are **not** computed
from a local book, and fills do not automatically change cash/positions.
`script_order` applies patches on successive detail lookups (by either ID), then
retains the last snapshot. Lists reflect the last observed snapshot without
advancing the script. Use this for partial fills, fills, expiry, rejection, or
unknown statuses. Cancellation clears pending patches.

`next_place_reply(Status(500))` fails one POST without storing it;
`next_place_reply(Disconnect)` stores it but loses the response. Lookup by
`client_order_id` then recovers that exact order. Reposting the ID returns 422.
Use recorded requests to prove the **client** does not retry POST or DELETE.

This is a scoped test double, not a matching engine, market clock, risk engine,
or full Alpaca emulator. Extended hours and non-day/non-limit requests are rejected.
The ticket explicitly calls for a notional-sized limit variant, so the harness
accepts it. The [Alpaca create-order reference](https://docs.alpaca.markets/reference/postorder)
(checked 2026-11-10) currently documents `notional` for market/day orders only.
Passing that fixture is not proof Alpaca accepts notional-sized limit orders.

## Dependent suite adoption

At the base revision, only the `live-robinhood` read client and paper loop exist.
RM-2460 (Alpaca client), RM-2462 (durable book), and RM-2463 (`paper = false`
unlock) have not landed. Their suites must import this module and exercise their
real entry points rather than create new venue mocks. The `live-alpaca` feature
currently enables these harness contract tests only; it does not enable live
trading, replace the existing client, or bypass the paper-mode refusal.

The contract tests use reqwest directly to validate the stand-in independently.
They do **not** claim to validate the missing client, book, or unlock logic.
RM-2454's final cross-suite acceptance remains blocked until those suites exist
and are wired to this implementation.
