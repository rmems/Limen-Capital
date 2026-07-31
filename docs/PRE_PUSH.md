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
- [ ] Limen-Neural libraries remain separate checkouts (or git deps)
- [ ] `git status` clean of secrets, runs, jsonl
- [ ] Local: `cargo test` + `julia --project=brain test/runtests.jl`

## Suggested first commit scope

Source + docs + fixtures + lockfile; no `target/`, no agent tooling, no vault DBs.

## Clone layout for collaborators

```text
git clone <your-remote> Limen-Capital
# optional libraries (public Limen-Neural repos or your mirrors):
git clone https://github.com/Limen-Neural/metabolic-ledger Limen-Neural/metabolic-ledger
git clone https://github.com/Limen-Neural/corpus-ipc Limen-Neural/corpus-ipc
# …
export LIMEN_NEURAL=$PWD/Limen-Neural
```

Path deps in Cargo/Project.toml assume `Limen-Neural` next to `Limen-Capital` by default.
