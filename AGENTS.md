# Stellaris Repository Instructions

Every task in this repository starts by reading this file and the
[AI iteration playbook](docs/ai-iteration-playbook.md) completely. Then read the
[documentation center](docs/README.md), the
[change workflow](docs/documentation-workflow.md), and the complete Current or
Proposed design and ADRs relevant to the task. Read each of them in full.

Before editing, testing, or committing, reconstruct the state from Git and the
authoritative documents. Report the current branch, full HEAD, worktree and
staging state, six-dimensional lifecycle state, authoritative design and ADRs,
change class, PR purpose, and target Gate ID (or `N/A` with a reason). Do not
reuse a progress summary from an earlier session.

All behavior-changing work in this repository is documentation-driven.

1. Classify the change as D0-D4 using the workflow. D0/D1 may follow an
   existing Current contract. D2-D4 require a Design-only change first.
2. Do not implement a D2-D4 change until its Proposed design is Approved, its
   approval commit and atomic Gate IDs are recorded, and every required ADR is
   Accepted.
3. Treat the Chinese documents under `docs/` as authoritative. Keep Current,
   Proposed, Historical, delivery, verification, and release states separate.
4. Work on one small, buildable, testable, reversible slice mapped to one Gate.
   D0/D1 may use `Gate: N/A` with a concrete reason. The only multi-Gate
   exception is an Evidence-only record or D4 status projection whose
   authoritative workflow requires the exact frozen Gate set. Do not include
   unrelated refactors or another implementation Gate in the same iteration.
5. Preserve all pre-existing worktree and staged changes as user-owned. Do not
   overwrite, revert, stage, or commit them. If overlapping changes cannot be
   isolated, do not commit. Validate the exact staged tree independently when
   excluded user changes remain; a combined dirty-worktree pass is insufficient.
6. Do not add v1/v2 compatibility, migration, dual-stack listeners, protocol
   downgrade, or hidden legacy input support unless a new Accepted ADR changes
   that rule. Do not inspect, switch to, compare with, or copy from the `v6`
   branch unless the user explicitly requests it in the current task.
7. Never describe a capability as verified or supported without evidence for
   the exact target commit in `docs/verification/`.
8. Keep the affected authoritative documents, examples, tests, deployment
   guidance, and `CHANGELOG.md` synchronized according to the documentation
   impact matrix.
9. ADR `Accepted` and design `Approved` require the user's explicit, scoped
   authorization. "Continue", "next", or use of the playbook is not approval.

Before handing off a change, run the applicable build, lint, test, and rustdoc
commands from `CONTRIBUTING.md` plus:

```bash
cargo fmt --all -- --check
ruby scripts/test-pr-design-contract.rb
ruby scripts/test-release-evidence.rb
ruby scripts/test-release-workflow.rb
bash scripts/check-docs.sh
git diff --check
```

List any required checks that could not be run. Do not create verification
records for uncommitted work. In Evidence-only work, preserve failed, skipped,
or unexecuted outcomes as `Failed` or `Partial` exactly as required by
`docs/verification/README.md`; never record them as `Passed` or use them to
advance verification state.

When all required repository checks and change-integrity tests pass, create a
Conventional Commit containing only the current iteration. Non-Evidence-only
behavior tests must also pass. A target Gate's accurate `Failed` or `Partial`
outcome may be committed only in Evidence-only work and is not a repository
check failure. Do not commit when required checks fail or scope is unclear.
Never automatically push, merge, tag, release, amend, rebase, force-push, or
otherwise rewrite history. Finish with the commit SHA, changed scope, checks
run, gates not run, residual risks, and the next legal slice.
