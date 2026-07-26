# Stellaris Repository Instructions

All behavior-changing work in this repository is documentation-driven.

1. Read the [documentation center](docs/README.md) and
   [change workflow](docs/documentation-workflow.md) before changing code,
   configuration, protocol, persistence, security, deployment, or release
   behavior.
2. Classify the change as D0-D4 using the workflow. D0/D1 may follow an
   existing Current contract. D2-D4 require a Design-only change first.
3. Do not implement a D2-D4 change until its Proposed design is Approved, its
   approval commit and atomic Gate IDs are recorded, and every required ADR is
   Accepted.
4. Treat the Chinese documents under `docs/` as authoritative. Keep Current,
   Proposed, Historical, delivery, verification, and release states separate.
5. Do not add v1/v2 compatibility, migration, dual-stack listeners, protocol
   downgrade, or hidden legacy input support unless a new Accepted ADR changes
   that rule.
6. Never describe a capability as verified or supported without evidence for
   the exact target commit in `docs/verification/`.
7. Keep the affected authoritative documents, examples, tests, deployment
   guidance, and `CHANGELOG.md` synchronized according to the documentation
   impact matrix.

Before handing off a change, run the applicable code tests plus:

```bash
cargo fmt --all -- --check
ruby scripts/test-pr-design-contract.rb
ruby scripts/test-release-evidence.rb
ruby scripts/test-release-workflow.rb
bash scripts/check-docs.sh
git diff --check
```

List any required checks that could not be run. Do not create verification
records for uncommitted work or failed, skipped, or unexecuted gates.
