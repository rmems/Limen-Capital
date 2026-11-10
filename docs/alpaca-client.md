# Alpaca client (RM-2460)

This feature is separate from the research execution path. Enable it with
`--features live-alpaca`. `limen-live` still refuses `paper = false`, before
loading credentials or contacting Alpaca. No real orders are enabled by this CLI.

Credentials are read only from `APCA_API_KEY_ID` and `APCA_API_SECRET_KEY`.
Never put them in TOML, source files, CLI arguments, or the journal. Requests
use `APCA-API-KEY-ID` and `APCA-API-SECRET-KEY` headers, without signing.
Production trading requests use the fixed `https://api.alpaca.markets` endpoint;
quotes use `https://data.alpaca.markets` with the IEX feed. Neither endpoint
is configurable from a run file.

## Orders

`AlpacaClient::place_limit_order` sends one `POST /v2/orders`, with a UUID
`client_order_id` preserved from `risk::check`. Convert an `ApprovedOrder`
with `PlaceLimitOrderRequest::from(&approved)` to preserve rounded quantities
and prices. `LimitSize` represents exactly one of `qty` or `notional`.
Orders are limit-only, `day`, and regular-session only (`extended_hours = false`).

The request model supports both size fields as specified by this slice.
Alpaca's current Create Order reference restricts notional orders to market
orders, so a notional limit request may receive a venue rejection. The
risk-approved conversion uses fractional `qty`, not `notional`; no fallback
to market orders is allowed.

No POST is retried. A transport failure, malformed success response, timeout,
or server error triggers `GET /v2/orders:by_client_order_id`. If the lookup
also fails, `SubmissionUnknown` preserves the UUID for later reconciliation.
A lookup returning 404 does not prove the POST failed: never place a replacement
order based on that result alone.

`cancel_order` sends one `DELETE /v2/orders/{id}` and accepts an empty 204.
A successful DELETE acknowledges the request, not a confirmed cancellation.
Use polling to observe the final outcome, including fills racing with cancel.

`poll_order` is bounded by a caller-supplied poll count and interval. It returns
the latest order, whether terminal or still open. Filled, canceled, expired,
and rejected stop polling. An unknown status is neither terminal nor a fill.
`filled_qty` is Alpaca's cumulative total of partial fills for that order;
do not sum successive snapshots. To total fills across distinct orders, sum
their latest `filled_qty` once per venue order ID.

Equity orders fill in session. A regular-session day order left open at close
can expire or be canceled; both are normal terminal outcomes. There is no local
clock-based fill or expiry inference. Orders submitted after close can remain
queued for the next session. Extended-hours execution is not enabled.

`get_orders` requests `status=all`, returning the latest 500 orders, not an
exhaustive historical ledger. Track known orders with `get_order` instead.

## Paper CLI

The default symbol is `AAPL`; the small equity/ETF allowlist is `AAPL`, `SPY`.
The paper fee remains an explicitly configurable simulation cost, not an Alpaca
fee estimate. An offline run needs no credentials:

```toml
paper = true
symbol = "AAPL"
journal_path = "journal.md"

[market]
source = "synthetic"
start_price = 200.0
```

```sh
cd execution
cargo run --locked --features live-alpaca --bin limen-live -- --config /absolute/path/paper.toml --once
cargo test --locked --features live-alpaca
```

Live quotes in paper mode use `[market] source = "alpaca"` (the default) and
require the environment pair. The venue account/asset metadata is diagnostic;
the paper book starts from configured simulated cash, not real holdings.
