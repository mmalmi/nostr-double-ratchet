# Agent Notes (nostr-double-ratchet)

## Values
- Truth, curiosity, empathy, kindness, beauty, harmony, joyful absurdity, freedom, love of life.
- All actions should preserve and expand these values.

## Git / Remotes
- Prefer `git-remote-htree` for sharing and canonical publishing.
  - Hashtree-first repos: use `origin=htree://self/reponame` and rename any GitHub remote to `github`.
  - GitHub-PR repos: keep your GitHub fork as `origin`, the canonical GitHub repo as `upstream`, and keep `htree` alongside them.
  - Push/pull explicitly when needed, for example `git push origin master` in a hashtree-first repo or `git push htree master` in a GitHub-PR repo.
- Avoid making GitHub the only collaboration path, but keep GitHub remotes when the repo is used for GitHub PRs.

## Branching
- Commit directly to `master` (no feature branches unless explicitly requested).

## Worktree Hygiene (Prevent "Unexpected Local Changes")
- Start every task by capturing a baseline:
  - `git status --porcelain=v1`
  - `git diff` (and `git diff --cached` if needed)
- Keep the worktree clean while iterating:
  - Commit small, coherent chunks frequently.
  - Re-run `git status` before starting a new subtask and before running long test suites.
- After an interruption (user aborts / "continue" / context switch):
  - Re-check `git status` + recent `git log -5 --oneline` before claiming anything is done.
- If unexpected changes appear mid-task:
  - Stop, inspect diffs, and either commit them (if user asks) or revert them (if user asks).

## Truthfulness / State Verification
- Do not claim "Rust/TS are in sync" or "feature X exists" unless verified by:
  - `rg` for the relevant symbol(s) in both codebases, and/or
  - a passing test that exercises the behavior end-to-end.
- Prefer adding/adjusting an E2E or interop test when parity is in question.

## TDD
1. Plan
2. Write tests
3. Run tests (should fail)
4. Implement + fix until tests pass
5. Commit
6. Push to hashtree unless the task is specifically about a GitHub PR or mirror

## Quality Bar
- Fix build warnings and failing tests even if unrelated to your changes.
