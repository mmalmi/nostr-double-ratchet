# Agent Notes (nostr-double-ratchet)

Values: truth, curiosity, empathy, kindness, beauty, harmony, joyful absurdity, freedom, love of life. Preserve and expand them.

## Git / Remotes

- Prefer `git-remote-htree` for sharing/canonical publishing. Hashtree-first repos: `origin=htree://self/reponame`, GitHub remote renamed `github`. GitHub-PR repos: `origin` = fork, `upstream` = canonical GitHub, keep `htree` alongside. Push/pull explicitly when needed, e.g. `git push origin master` for hashtree-first or `git push htree master` for GitHub-PR repos.
- Avoid GitHub-only collaboration, but keep GitHub remotes for GitHub PR repos.
- Commit directly to `master`; no feature branches unless explicitly requested.

## Worktree Hygiene

- Start tasks with `git status --porcelain=v1`, `git diff`, and `git diff --cached` if needed.
- Keep the worktree clean: commit small coherent chunks, rerun `git status` before new subtasks and long test suites.
- After interruption/continue/context switch, re-check `git status` and `git log -5 --oneline` before claiming completion.
- If unexpected changes appear mid-task, stop and inspect diffs; commit them only if user asks, revert them only if user asks.

## Verification / TDD / Quality

- Do not claim Rust/TS sync or feature existence unless verified by `rg` in both codebases and/or a passing end-to-end test. Prefer adding/adjusting E2E or interop tests for parity questions.
- TDD: plan, write tests, run them and see failure, implement/fix until passing, commit, then push to hashtree unless the task is specifically a GitHub PR/mirror.
- Fix build warnings and failing tests even when unrelated.
- Never publish a message recipient's long-term public key in double ratchet message events.
