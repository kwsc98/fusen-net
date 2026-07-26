## Summary

Describe the user-visible behavior and why the change is needed.

## Design contract

- Change class:
- PR purpose:
- Proposed design:
- Required ADR(s):
- Design review status and approval commit:
- Atomic Gate ID(s):
- Evidence record(s):
- Current authoritative documents affected:
- Applicability, design-review, ADR, delivery, verification, and release status changes:

Use exactly `Design-only`, `Implementation`, or `Evidence-only` for PR purpose.
D0/D1 always use `Implementation` and state `Not applicable: <reason>` for
design-only and evidence fields. D0 may change Markdown only; neither D0 nor D1
may change Proposed designs or ADR records.
A D2-D4 `Design-only` PR may add or advance declared design/ADR records, but new
records must remain `Not started`, `Unverified`, and `Unreleased`, and those
three states may not change in that PR mode. It may change only declared records
and the approved docs index files. A D2-D4 `Implementation` PR must link a repository-local Approved design,
its full 40-character approval commit, and an atomic Gate ID frozen in that
design. D2 must write `Not required: <reason>` for ADRs; if an ADR trigger
applies, reclassify as D3. D3-D4 Implementation must link an Accepted ADR.
Implementation prerequisites cannot be waived, and Implementation cannot add
verification records or advance verification/release state.
For D0-D3 Implementation and D4 work performed before evidence exists, write
`Not applicable: <reason>` in `Evidence record(s)`. A D4 Implementation whose
actual diff changes only content inside pre-existing allowlisted
`stellaris-release-status` markers, plus optional root `README.md`,
`README.en.md`, or `CHANGELOG.md` projections, must instead list one or more
canonical `docs/verification/...md` records already present in the PR base.
At least one authoritative marker must change; root documents cannot advance a
support or Stable claim alone. Adding a marker or changing marker-document text
outside one is not a status projection and requires new evidence later. D4
Implementation may reference only merged, immutable records; it may not add,
edit, rename, or delete them. All records used by a status projection must
share one target commit that is an ancestor of the PR base and remains current.
Every record must be `Passed` with `豁免：None`; their non-duplicated Gate
rows must exactly match both the PR's Atomic Gate IDs and every Gate frozen in
the Approved design. Each Gate must remain the latest Passed, unwaived result
in first-parent addition order; a later Failed, Partial, or waived result
invalidates an older pass.

`Evidence-only` always uses D4. It must add immutable records under
`docs/verification/`, bind them to a commit already in the PR base history, and
list exactly the Gate rows in those records. It may change only verification,
release, and last-check metadata in the declared design, ADRs, and Current
documents; source, configuration, Current contract text, and existing evidence
records are forbidden. Every D2-D4 design has one machine-readable `关联 ADR`;
the PR ADR set must match it exactly, and an Approved design requires every
associated ADR to be Accepted.
Evidence records use `豁免: None` when no waiver exists. For each target/Gate,
the latest record in first-parent addition order replaces earlier results;
Verified and Stable require that latest result to be Passed with no waiver.

After Evidence-only advances machine-readable lifecycle metadata, synchronize
README, compatibility tables, deployment text, and release notes in a separate
documentation-only D4 Implementation PR. That follow-up must reference the
merged evidence, must not change lifecycle metadata, and must not include
runtime or configuration changes.

## Impact

Describe breaking wire/configuration/state behavior, security and trust
boundaries, persistence/recovery effects, and operational consequences.

## Validation

List the exact commands, environments, scenarios, duration, and results. List
required gates that were not run or still fail. Link immutable CI runs or
artifacts when claiming a gate passed.

## Checklist

- [ ] I added or updated tests for behavior changes.
- [ ] I classified the change under `docs/documentation-workflow.md` and updated every applicable authoritative document.
- [ ] I selected `Design-only`, `Implementation`, or `Evidence-only` and did not mix their change scopes.
- [ ] D2-D4 Implementation links an Approved design, full approval commit, atomic Gate ID, and every required Accepted ADR.
- [ ] Applicability, design review, ADR, delivery, verification, and release states remain separate.
- [ ] Evidence-only records are new, target a commit already in the PR base history, use the canonical Gate table, and do not rewrite prior evidence.
- [ ] A marker-scoped D4 status projection and any root summary projections list merged, current, latest Passed, unwaived evidence covering every frozen Gate; pre-evidence contract or marker changes use `Not applicable` and invalidate older evidence.
- [ ] `cargo fmt --all -- --check`, Clippy, relevant tests, `bash scripts/check-docs.sh`, and `git diff --check` pass.
- [ ] I did not commit credentials, private keys, tokens, packet captures, or packet payloads.
- [ ] New dependencies have an accepted license and a clear purpose.
- [ ] Breaking wire/configuration/state changes include an ADR, new versions, old-input rejection tests, and a changelog entry.
- [ ] I did not add a migrator, dual-stack listener, downgrade, or compatibility feature unless an accepted ADR explicitly requires it.
