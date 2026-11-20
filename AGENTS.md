# Agent Instructions

See @CLAUDE.md for additional repository context. The @-mention makes Amp load it; Amp reads `CLAUDE.md` on its own only when no `AGENTS.md` exists.

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

## PR metadata hygiene — set at creation, not as a retrofit

Every pull request opened in this repo must carry the following PR metadata, in the same pass it is created. If an item does not apply or cannot be set, state the exception in the PR body before treating the PR as complete:

- **Assignee:** `rmems`.
- **Linear issue ID** in the PR title as a trailing `(RM-xxxx)` or `(LIM-xxxx)`, in the branch name, and in commit subjects — so the GitHub↔Linear integration attaches the PR to its issue natively.
- **Labels** from this repo's existing label vocabulary — mirror the labels on the issue the PR implements; use only existing labels and note in the PR body when none apply.
- **Milestone:** the repo's milestone for the work, when this repo uses milestones and one applies.
- **Issue link:** the PR body references the issue it implements; when an issue is closed, its implementing PR is cited by full URL in the close comment and mirrored on the Linear twin.

Verify with a read after creating the PR. A PR missing applicable metadata without a stated exception is not done.
