# limen-live journal

- mode: paper | symbol: BTC-USD | decider: claude

| ts (UTC) | tick | symbol | bid | ask | mid | spread bps | pos qty | pos $ | cash $ | realized $ | decider | action | size $ | reason | risk | order | fill | pnl $ | llm $ |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 2026-11-09T00:00:00Z | 1 | BTC-USD | 100 | 100 | 100 | 0 | 0 | 0 | 10 | 0 | claude | hold | 0 | decider_error: parse_error bad JSON \| hold | hold | - | - | 0 | 0.001 |
| 2026-11-09T00:15:00Z | 2 | BTC-USD | 100 | 100 | 100 | 0 | 0 | 0 | 10 | 0 | claude | buy | 5 | buy | approved | paper-1:open | - | 0 | 0.001 |
| 2026-11-09T00:30:00Z | 3 | BTC-USD | 99 | 99 | 99 | 0 | 0.05 | 4.95 | 4.9775 | 0 | claude | buy | 20 | too big | rejected:order_cap | paper-1:filled | buy 0.05000000 @ 100.00 fee 0.0225 | 0 | 0.001 |
| 2026-11-09T00:45:00Z | 4 | BTC-USD | 98 | 98 | 98 | 0 | 0.05 | 4.9 | 4.9775 | 0 | claude | hold | 0 | hold | hold | - | - | 0 | 0.001 |
| 2026-11-09T01:00:00Z | 5 | BTC-USD | 100 | 100 | 100 | 0 | 0.05 | 5 | 4.9775 | 0 | claude | sell | 5 | exit | approved | paper-2:open | - | 0 | 0.001 |
| 2026-11-09T01:15:00Z | 6 | BTC-USD | 101 | 101 | 101 | 0 | 0 | 0 | 9.955 | -0.045 | claude | hold | 0 | done | hold | paper-2:filled | sell 0.05000000 @ 100.00 fee 0.0225 | -0.045 | 0.001 |
