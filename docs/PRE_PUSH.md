# Pre-push checklist (this repo is not under Limen-Neural org)

Use before creating/updating a **personal** GitHub remote (e.g. `rmems/...`).

## Must be true

- [x] `.mimocode/` ignored
- [x] `wire/fixtures/*.bin` tracked (`!wire/fixtures/**`)
- [x] No `/home/raulmc/...` vault path (`LIMEN_VAULT_DIR` or `data/vault`)
- [x] ZMQ CURVE keys only from env
- [x] `execution/target/` ignored
- [x] `quantum_navigator.jl` removed
- [x] README states this is **not** the Limen-Neural org
- [ ] Remote URL is **your** user/org — not `github.com/Limen-Neural/...`
- [x] Limen-Neural deps are **git+rev** only (no sibling path clones required)
- [ ] `git status` clean of secrets, runs, jsonl
- [ ] Local: `cargo test` + `julia --project=brain test/runtests.jl`

## Suggested first commit scope

Source + docs + fixtures + lockfile; no `target/`, no agent tooling, no vault DBs.

## Clone for collaborators

```text
git clone <your-remote> Limen-Capital
cd Limen-Capital/execution && cargo test   # fetches validated LN crates by rev
cd ../brain && julia --project=. -e 'using Pkg; Pkg.instantiate()'
julia --project=. test/runtests.jl
```

Pins and package list: **[docs/deps.md](deps.md)**. No `Limen-Neural/` sibling tree required.
