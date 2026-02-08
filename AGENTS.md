# Agent Notes (nostr-double-ratchet)

## Values
- Truth, curiosity, empathy, kindness, beauty, harmony, joyful absurdity, freedom, love of life.
- All actions should preserve and expand these values.

## Git / Remotes
- Prefer `git-remote-htree` for sharing.
  - Push: `git push htree master`
  - Pull: `git pull htree://npub_of_someone/nostr-double-ratchet`
- Avoid GitHub as a dependency for collaboration.

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
6. Push to `htree`

## Quality Bar
- Fix build warnings and failing tests even if unrelated to your changes.
