#!/usr/bin/env ruby
# SPDX-License-Identifier: Apache-2.0 OR MIT

require "fileutils"
require "open3"
require "rbconfig"
require "tmpdir"

CHECKER = File.expand_path("check-pr-design-contract.rb", __dir__)
GATE_ID = "TEST-P1-CONTRACT-01"
SECOND_GATE_ID = "TEST-P1-CONTRACT-02"
DESIGN_PATH = "docs/test-plan.md"
ADR_PATH = "docs/adr/0099-test-decision.md"
SECOND_ADR_PATH = "docs/adr/0098-second-decision.md"
CI_WORKFLOW_PATH = File.expand_path("../.github/workflows/ci.yml", __dir__)

def validate_ci_checker_order!
  workflow = File.read(CI_WORKFLOW_PATH)
  fragments = [
    'git show "${PR_BASE_SHA}:scripts/check-pr-design-contract.rb"',
    'git show "${PR_BASE_SHA}:scripts/docs_metadata.rb"',
    'ruby "${base_checker_dir}/check-pr-design-contract.rb"',
    "ruby scripts/check-pr-design-contract.rb"
  ]
  positions = fragments.map do |fragment|
    workflow.index(fragment) || abort("CI must run the protected base checker before HEAD; missing #{fragment}")
  end
  abort "CI must run the protected base checker before the HEAD checker" unless positions.each_cons(2).all? { |left, right| left < right }
end

validate_ci_checker_order!

def run_command(*command, chdir:, env: {})
  stdout, stderr, status = Open3.capture3(env, *command, chdir: chdir)
  [stdout, stderr, status]
end

def git!(repo, *arguments)
  stdout, stderr, status = run_command("git", *arguments, chdir: repo)
  raise "git #{arguments.join(' ')} failed: #{stderr}" unless status.success?
  stdout.strip
end

def write_file(repo, path, contents)
  absolute = File.join(repo, path)
  FileUtils.mkdir_p(File.dirname(absolute))
  File.write(absolute, contents)
end

def commit!(repo, message)
  git!(repo, "add", "-A")
  git!(repo, "commit", "-q", "-m", message)
  git!(repo, "rev-parse", "HEAD")
end

def with_repo
  Dir.mktmpdir("stellaris-pr-contract-") do |repo|
    git!(repo, "init", "-q")
    git!(repo, "config", "user.name", "Contract Test")
    git!(repo, "config", "user.email", "contract-test@example.invalid")
    write_file(repo, "README.md", "# Fixture\n")
    base = commit!(repo, "initial fixture")
    yield repo, base
  end
end

def design_document(
  review: "Draft",
  approval: "N/A",
  adr_status: "N/A",
  applicability: "Proposed",
  delivery: "Not started",
  verification: "Unverified",
  release: "Unreleased",
  related_adrs: nil,
  body: "Frozen behavior.",
  gate_ids: [GATE_ID]
)
  related_adrs ||= adr_status == "N/A" ? "N/A: no authoritative ADR trigger applies" : ADR_PATH
  adr_reference = adr_status == "N/A" ? "No ADR applies." : "Decision: #{File.basename(ADR_PATH)}."
  gate_rows = gate_ids.map { |gate_id| "| `#{gate_id}` | Open |" }.join("\n")
  <<~MARKDOWN
    # Test design

    - 文档适用性：#{applicability}
    - 适用范围：test fixture
    - 设计评审状态：#{review}
    - 设计批准：#{approval}
    - 关联 ADR：#{related_adrs}
    - ADR 决策状态：#{adr_status}
    - 交付状态：#{delivery}
    - 验证状态：#{verification}
    - 发布状态：#{release}

    #{adr_reference}

    ## Contract

    #{body}

    ## Gates

    | Gate ID | Result |
    | --- | --- |
    #{gate_rows}
  MARKDOWN
end

def evidence_document(target:, overall: "Passed", waiver: "None", gates: { GATE_ID => "Passed" })
  rows = gates.map do |gate_id, result|
    "| `#{gate_id}` | exact fixture command | expected outcome | observed outcome | #{result} | sha256:#{'a' * 64} |"
  end.join("\n")
  <<~MARKDOWN
    # Test verification record

    - 文档适用性：Current
    - 适用范围：contract fixture evidence
    - 设计评审状态：N/A
    - ADR 决策状态：N/A
    - 交付状态：N/A
    - 验证状态：N/A
    - 发布状态：N/A
    - 证据目标：#{target}
    - 总体结果：#{overall}
    - 豁免：#{waiver}

    | Gate ID | 命令或场景 | 预期 | 实际 | 结果 | Artifact / hash |
    | --- | --- | --- | --- | --- | --- |
    #{rows}
  MARKDOWN
end

def evidence_path(target, scope: "test")
  "docs/verification/2026-07-26-#{scope}-#{target[0, 7]}.md"
end

def status_projection_document(status:, contract: "Frozen documentation contract.")
  <<~MARKDOWN
    # Documentation status fixture

    - 文档适用性：Current
    - 适用范围：status projection fixture
    - 设计评审状态：N/A
    - ADR 决策状态：N/A
    - 交付状态：Implemented
    - 验证状态：Unverified
    - 发布状态：Unreleased

    <!-- stellaris-release-status:documentation-status:start -->
    #{status}
    <!-- stellaris-release-status:documentation-status:end -->

    ## Frozen contract

    #{contract}
  MARKDOWN
end

def adr_document(
  status: "Proposed",
  applicability: "Proposed",
  delivery: "Not started",
  verification: "Unverified",
  release: "Unreleased",
  body: "Frozen decision."
)
  <<~MARKDOWN
    # ADR 0099: Test decision

    - 文档适用性：#{applicability}
    - 适用范围：test fixture
    - 设计评审状态：N/A
    - ADR 决策状态：#{status}
    - 交付状态：#{delivery}
    - 验证状态：#{verification}
    - 发布状态：#{release}

    ## Decision

    #{body}
  MARKDOWN
end

def pr_body(
  change_class:,
  purpose:,
  design: DESIGN_PATH,
  adr: "Not required: no authoritative ADR trigger applies",
  review: "Draft; approval pending",
  gates: GATE_ID,
  evidence: "Not applicable: no evidence status transition",
  current_docs: "docs/protocol.md after implementation"
)
  <<~MARKDOWN
    ## Summary

    Contract fixture.

    ## Design contract

    - Change class: #{change_class}
    - PR purpose: #{purpose}
    - Proposed design: #{design}
    - Required ADR(s): #{adr}
    - Design review status and approval commit: #{review}
    - Atomic Gate ID(s): #{gates}
    - Evidence record(s): #{evidence}
    - Current authoritative documents affected: #{current_docs}
    - Applicability, design-review, ADR, delivery, verification, and release status changes: fixture transition only

    ## Impact

    Fixture impact.

    ## Validation

    This checker is the validation.

    ## Checklist

    - [x] Fixture complete.
  MARKDOWN
end

def run_checker(repo, body, base)
  run_command(
    RbConfig.ruby,
    CHECKER,
    chdir: repo,
    env: { "PR_BODY" => body, "PR_BASE_SHA" => base }
  )
end

def expect_success(repo, body, base)
  stdout, stderr, status = run_checker(repo, body, base)
  return if status.success?
  raise "expected success, got #{status.exitstatus}: #{stderr}#{stdout}"
end

def expect_failure(repo, body, base, message)
  stdout, stderr, status = run_checker(repo, body, base)
  raise "expected failure, checker succeeded: #{stdout}" if status.success?
  output = "#{stderr}#{stdout}"
  raise "expected #{message.inspect}, got #{output.inspect}" unless output.include?(message)
end

def approved_fixture(repo, gate_ids: [GATE_ID])
  write_file(repo, ADR_PATH, adr_document(status: "Accepted"))
  write_file(repo, DESIGN_PATH, design_document(adr_status: "Accepted", gate_ids: gate_ids))
  frozen_sha = commit!(repo, "freeze accepted design")
  approval = "reviewer@example.invalid + approval commit #{frozen_sha}"
  write_file(
    repo,
    DESIGN_PATH,
    design_document(
      review: "Approved",
      approval: approval,
      adr_status: "Accepted",
      delivery: "In progress",
      gate_ids: gate_ids
    )
  )
  write_file(repo, ADR_PATH, adr_document(status: "Accepted", delivery: "Implemented"))
  approved_base = commit!(repo, "record approval")
  [frozen_sha, approval, approved_base]
end

def implemented_fixture(repo)
  frozen_sha, approval, _approved_base = approved_fixture(repo)
  write_file(
    repo,
    DESIGN_PATH,
    design_document(
      review: "Approved",
      approval: approval,
      adr_status: "Accepted",
      delivery: "Implemented"
    )
  )
  write_file(repo, "src/lib.rs", "pub fn implemented_behavior() {}\n")
  target = commit!(repo, "implement approved behavior")
  [frozen_sha, approval, target]
end

tests = {
  "D0 Implementation remains valid" => lambda do
    with_repo do |repo, base|
      body = pr_body(
        change_class: "D0",
        purpose: "Implementation",
        design: "Not applicable: typo-only correction",
        adr: "Not applicable: no behavior change",
        review: "Not applicable: no design review",
        gates: "Not applicable: no behavior gate"
      )
      expect_success(repo, body, base)
    end
  end,
  "D0 rejects source changes" => lambda do
    with_repo do |repo, base|
      write_file(repo, "src/lib.rs", "pub fn behavior_change() {}\n")
      commit!(repo, "mislabel behavior change as D0")
      body = pr_body(
        change_class: "D0",
        purpose: "Implementation",
        design: "Not applicable: claimed typo-only correction",
        adr: "Not applicable: claimed no behavior change",
        review: "Not applicable: claimed no design review",
        gates: "Not applicable: claimed no behavior gate"
      )
      expect_failure(repo, body, base, "D0 is documentation-only")
    end
  end,
  "D1 rejects Proposed design changes" => lambda do
    with_repo do |repo, _initial|
      write_file(repo, DESIGN_PATH, design_document)
      base = commit!(repo, "merge proposed design")
      write_file(repo, DESIGN_PATH, design_document(body: "Changed during a claimed bug fix."))
      commit!(repo, "change proposal during D1")
      body = pr_body(
        change_class: "D1",
        purpose: "Implementation",
        design: "Not applicable: claimed current-contract bug fix",
        adr: "Not applicable: claimed no ADR trigger",
        review: "Not applicable: claimed no design review",
        gates: "Not applicable: regression belongs to current contract"
      )
      expect_failure(repo, body, base, "D1 may not change Proposed design or ADR records")
    end
  end,
  "D0 cannot seed a Proposed design outside Design-only" => lambda do
    with_repo do |repo, base|
      write_file(repo, DESIGN_PATH, design_document)
      commit!(repo, "seed proposal as D0")
      body = pr_body(
        change_class: "D0",
        purpose: "Implementation",
        design: "Not applicable: claimed new documentation",
        adr: "Not applicable: claimed no behavior change",
        review: "Not applicable: claimed no design review",
        gates: "Not applicable: claimed no behavior gate"
      )
      expect_failure(repo, body, base, "D0 may not change Proposed design or ADR records")
    end
  end,
  "D1 cannot change ADR lifecycle" => lambda do
    with_repo do |repo, _initial|
      write_file(repo, ADR_PATH, adr_document)
      base = commit!(repo, "merge proposed ADR")
      write_file(repo, ADR_PATH, adr_document(status: "Accepted"))
      commit!(repo, "accept ADR as D1")
      body = pr_body(
        change_class: "D1",
        purpose: "Implementation",
        design: "Not applicable: claimed bug fix",
        adr: "Not applicable: claimed no ADR trigger",
        review: "Not applicable: claimed no design review",
        gates: "Not applicable: claimed current regression"
      )
      expect_failure(repo, body, base, "D1 may not change Proposed design or ADR records")
    end
  end,
  "D0 cannot rewrite an existing verification record" => lambda do
    with_repo do |repo, _initial|
      target = git!(repo, "rev-parse", "HEAD")
      record_path = evidence_path(target)
      write_file(repo, record_path, evidence_document(target: target))
      base = commit!(repo, "merge immutable evidence")
      write_file(
        repo,
        record_path,
        evidence_document(target: target, overall: "Failed", gates: { GATE_ID => "Failed" })
      )
      commit!(repo, "rewrite evidence as D0")
      body = pr_body(
        change_class: "D0",
        purpose: "Implementation",
        design: "Not applicable: claimed evidence typo",
        adr: "Not applicable: claimed no behavior change",
        review: "Not applicable: claimed no design review",
        gates: "Not applicable: claimed no behavior gate"
      )
      expect_failure(repo, body, base, "verification records are immutable")
    end
  end,
  "D0 cannot add a verification record" => lambda do
    with_repo do |repo, base|
      record_path = evidence_path(base)
      write_file(repo, record_path, evidence_document(target: base))
      commit!(repo, "smuggle evidence through D0")
      body = pr_body(
        change_class: "D0",
        purpose: "Implementation",
        design: "Not applicable: claimed evidence documentation",
        adr: "Not applicable: claimed no behavior change",
        review: "Not applicable: claimed no design review",
        gates: "Not applicable: claimed no behavior gate"
      )
      expect_failure(repo, body, base, "new verification records require an Evidence-only PR")
    end
  end,
  "Evidence-only cannot delete an existing verification record" => lambda do
    with_repo do |repo, _initial|
      target = git!(repo, "rev-parse", "HEAD")
      record_path = evidence_path(target)
      write_file(repo, record_path, evidence_document(target: target))
      base = commit!(repo, "merge immutable evidence")
      FileUtils.rm(File.join(repo, record_path))
      commit!(repo, "delete evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Evidence-only",
        adr: ADR_PATH,
        review: "Approved; #{target}",
        evidence: record_path,
        current_docs: "No Current contract documents; evidence deletion only"
      )
      expect_failure(repo, body, base, "verification records are immutable")
    end
  end,
  "D2 Design-only accepts a new Draft without an ADR" => lambda do
    with_repo do |repo, base|
      write_file(repo, DESIGN_PATH, design_document)
      commit!(repo, "add draft design")
      expect_success(repo, pr_body(change_class: "D2", purpose: "Design-only"), base)
    end
  end,
  "D3 Design-only accepts a Draft and Proposed ADR" => lambda do
    with_repo do |repo, base|
      write_file(repo, ADR_PATH, adr_document)
      write_file(repo, DESIGN_PATH, design_document(adr_status: "Proposed"))
      commit!(repo, "add design proposal")
      body = pr_body(change_class: "D3", purpose: "Design-only", adr: ADR_PATH)
      expect_success(repo, body, base)
    end
  end,
  "D3 Design-only accepts a standalone Proposed ADR" => lambda do
    with_repo do |repo, base|
      write_file(repo, ADR_PATH, adr_document)
      commit!(repo, "add ADR proposal")
      body = pr_body(
        change_class: "D3",
        purpose: "Design-only",
        design: "Not applicable: standalone ADR proposal",
        adr: ADR_PATH,
        review: "Not applicable: no detailed design in this PR",
        gates: "Not applicable: gates belong to the later detailed design"
      )
      expect_success(repo, body, base)
    end
  end,
  "Design-only accepts Proposed to Accepted and matching design metadata" => lambda do
    with_repo do |repo, _initial|
      write_file(repo, ADR_PATH, adr_document)
      write_file(repo, DESIGN_PATH, design_document(adr_status: "Proposed"))
      base = commit!(repo, "merge proposal")
      write_file(repo, ADR_PATH, adr_document(status: "Accepted"))
      write_file(repo, DESIGN_PATH, design_document(adr_status: "Accepted"))
      commit!(repo, "accept decision")
      body = pr_body(change_class: "D3", purpose: "Design-only", adr: ADR_PATH)
      expect_success(repo, body, base)
    end
  end,
  "Design-only accepts Draft to Approved with a frozen commit" => lambda do
    with_repo do |repo, _initial|
      write_file(repo, ADR_PATH, adr_document(status: "Accepted"))
      write_file(repo, DESIGN_PATH, design_document(adr_status: "Accepted"))
      base = commit!(repo, "freeze design")
      approval = "reviewer@example.invalid + approval commit #{base}"
      write_file(
        repo,
        DESIGN_PATH,
        design_document(review: "Approved", approval: approval, adr_status: "Accepted")
      )
      commit!(repo, "approve design")
      body = pr_body(
        change_class: "D3",
        purpose: "Design-only",
        adr: ADR_PATH,
        review: "Approved; #{base}"
      )
      expect_success(repo, body, base)
    end
  end,
  "Design-only rejects Approved while an associated ADR remains Proposed" => lambda do
    with_repo do |repo, _initial|
      write_file(repo, ADR_PATH, adr_document)
      write_file(repo, DESIGN_PATH, design_document(adr_status: "Proposed"))
      base = commit!(repo, "freeze design with proposed ADR")
      approval = "reviewer@example.invalid + approval commit #{base}"
      write_file(
        repo,
        DESIGN_PATH,
        design_document(review: "Approved", approval: approval, adr_status: "Proposed")
      )
      commit!(repo, "approve before accepting ADR")
      body = pr_body(
        change_class: "D3",
        purpose: "Design-only",
        adr: ADR_PATH,
        review: "Approved; #{base}"
      )
      expect_failure(repo, body, base, "requires every associated ADR to be Accepted")
    end
  end,
  "Design-only rejects an incomplete Required ADR declaration" => lambda do
    with_repo do |repo, _initial|
      write_file(repo, SECOND_ADR_PATH, adr_document)
      base = commit!(repo, "merge second proposed ADR")
      write_file(repo, ADR_PATH, adr_document)
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          adr_status: "Proposed",
          related_adrs: "#{SECOND_ADR_PATH}, #{ADR_PATH}"
        )
      )
      commit!(repo, "declare two required ADRs in design")
      body = pr_body(change_class: "D3", purpose: "Design-only", adr: ADR_PATH)
      expect_failure(repo, body, base, "Required ADR(s) must exactly match 关联 ADR")
    end
  end,
  "Design-only rejects an approval snapshot created inside the same PR" => lambda do
    with_repo do |repo, _initial|
      write_file(repo, ADR_PATH, adr_document(status: "Accepted"))
      write_file(repo, DESIGN_PATH, design_document(adr_status: "Accepted"))
      base = commit!(repo, "merge original draft")
      write_file(
        repo,
        DESIGN_PATH,
        design_document(adr_status: "Accepted", body: "Rewritten draft in PR.")
      )
      in_pr_snapshot = commit!(repo, "rewrite draft before approval")
      approval = "reviewer@example.invalid + approval commit #{in_pr_snapshot}"
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Approved",
          approval: approval,
          adr_status: "Accepted",
          body: "Rewritten draft in PR."
        )
      )
      commit!(repo, "approve rewritten draft")
      body = pr_body(
        change_class: "D3",
        purpose: "Design-only",
        adr: ADR_PATH,
        review: "Approved; #{in_pr_snapshot}"
      )
      expect_failure(repo, body, base, "must be an ancestor of PR base")
    end
  end,
  "Design-only retains a Rejected design and ADR as Historical" => lambda do
    with_repo do |repo, _initial|
      write_file(repo, ADR_PATH, adr_document)
      write_file(repo, DESIGN_PATH, design_document(adr_status: "Proposed"))
      base = commit!(repo, "merge proposal before rejection")
      write_file(
        repo,
        ADR_PATH,
        adr_document(status: "Rejected", applicability: "Historical")
      )
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Rejected",
          adr_status: "Rejected",
          applicability: "Historical"
        )
      )
      commit!(repo, "record rejection")
      body = pr_body(
        change_class: "D3",
        purpose: "Design-only",
        adr: ADR_PATH,
        review: "Rejected; retained for history"
      )
      expect_success(repo, body, base)
    end
  end,
  "Design-only rejects rewriting a proposal while marking it Rejected" => lambda do
    with_repo do |repo, _initial|
      write_file(repo, ADR_PATH, adr_document)
      write_file(repo, DESIGN_PATH, design_document(adr_status: "Proposed"))
      base = commit!(repo, "merge proposal before malicious rejection")
      write_file(
        repo,
        ADR_PATH,
        adr_document(status: "Rejected", applicability: "Historical")
      )
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Rejected",
          adr_status: "Rejected",
          applicability: "Historical",
          body: "Different history."
        )
      )
      commit!(repo, "rewrite and reject")
      body = pr_body(
        change_class: "D3",
        purpose: "Design-only",
        adr: ADR_PATH,
        review: "Rejected; retained for history"
      )
      expect_failure(repo, body, base, "finalized design body or immutable metadata may not be rewritten")
    end
  end,
  "Design-only retains Superseded finalized records without rewriting bodies" => lambda do
    with_repo do |repo, _initial|
      _frozen_sha, approval, base = approved_fixture(repo)
      write_file(
        repo,
        ADR_PATH,
        adr_document(
          status: "Superseded",
          applicability: "Historical",
          delivery: "Implemented"
        )
      )
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Superseded",
          approval: approval,
          adr_status: "Superseded",
          applicability: "Historical",
          delivery: "In progress"
        )
      )
      commit!(repo, "record supersession")
      body = pr_body(
        change_class: "D3",
        purpose: "Design-only",
        adr: ADR_PATH,
        review: "Superseded; retained for history"
      )
      expect_success(repo, body, base)
    end
  end,
  "D3 Implementation accepts frozen design and ADR bodies" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, approved_base = approved_fixture(repo)
      body = pr_body(
        change_class: "D3",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}"
      )
      expect_success(repo, body, approved_base)
    end
  end,
  "D4 status projection accepts merged immutable evidence" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(repo)
      write_file(
        repo,
        "docs/README.md",
        status_projection_document(status: "Unverified status.")
      )
      target = commit!(repo, "freeze status projection markers")
      record_path = evidence_path(target, scope: "projection")
      write_file(repo, record_path, evidence_document(target: target))
      base = commit!(repo, "merge evidence before projecting status")
      write_file(
        repo,
        "docs/README.md",
        status_projection_document(status: "Qualified status.")
      )
      write_file(repo, "README.md", "# Qualified root status projection\n")
      commit!(repo, "project qualified status")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "docs/README.md status projection only"
      )
      expect_success(repo, body, base)
    end
  end,
  "D4 status projection rejects Failed evidence" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(repo)
      write_file(repo, "docs/README.md", status_projection_document(status: "Unverified status."))
      target = commit!(repo, "freeze status projection markers")
      record_path = evidence_path(target, scope: "failed-projection")
      write_file(
        repo,
        record_path,
        evidence_document(target: target, overall: "Failed", gates: { GATE_ID => "Failed" })
      )
      base = commit!(repo, "merge failed evidence before projection")
      write_file(repo, "docs/README.md", status_projection_document(status: "Unsupported stable claim."))
      commit!(repo, "project status from failed evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "docs/README.md status projection only"
      )
      expect_failure(repo, body, base, "requires Overall Passed evidence")
    end
  end,
  "D4 status projection rejects Partial evidence" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(repo)
      write_file(repo, "docs/README.md", status_projection_document(status: "Unverified status."))
      target = commit!(repo, "freeze status projection markers")
      record_path = evidence_path(target, scope: "partial-projection")
      write_file(
        repo,
        record_path,
        evidence_document(target: target, overall: "Partial", gates: { GATE_ID => "Partial" })
      )
      base = commit!(repo, "merge partial evidence before projection")
      write_file(repo, "docs/README.md", status_projection_document(status: "Unsupported stable claim."))
      commit!(repo, "project status from partial evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "docs/README.md status projection only"
      )
      expect_failure(repo, body, base, "requires Overall Passed evidence")
    end
  end,
  "D4 status projection rejects waived evidence" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(repo)
      write_file(repo, "docs/README.md", status_projection_document(status: "Unverified status."))
      target = commit!(repo, "freeze status projection markers")
      record_path = evidence_path(target, scope: "waived-projection")
      write_file(
        repo,
        record_path,
        evidence_document(target: target, waiver: "issue-123 accepted limitation")
      )
      base = commit!(repo, "merge waived evidence before projection")
      write_file(repo, "docs/README.md", status_projection_document(status: "Unsupported stable claim."))
      commit!(repo, "project status from waived evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "docs/README.md status projection only"
      )
      expect_failure(repo, body, base, "requires unwaived evidence")
    end
  end,
  "D4 status projection rejects a stale evidence target" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(repo)
      write_file(repo, "docs/README.md", status_projection_document(status: "Unverified status."))
      target = commit!(repo, "freeze status projection markers")
      write_file(repo, "src/lib.rs", "pub fn changed_after_evidence() {}\n")
      record_path = evidence_path(target, scope: "stale-projection")
      write_file(repo, record_path, evidence_document(target: target))
      base = commit!(repo, "change runtime and merge stale evidence")
      write_file(repo, "docs/README.md", status_projection_document(status: "Unsupported stable claim."))
      commit!(repo, "project status from stale evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "docs/README.md status projection only"
      )
      expect_failure(repo, body, base, "stale because runtime/configuration changed")
    end
  end,
  "D4 status projection rejects an older Passed result superseded by Failed" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(repo)
      write_file(repo, "docs/README.md", status_projection_document(status: "Unverified status."))
      target = commit!(repo, "freeze status projection markers")
      passed_path = evidence_path(target, scope: "older-projection-pass")
      write_file(repo, passed_path, evidence_document(target: target))
      commit!(repo, "merge older passing evidence")
      failed_path = evidence_path(target, scope: "newer-projection-fail")
      write_file(
        repo,
        failed_path,
        evidence_document(target: target, overall: "Failed", gates: { GATE_ID => "Failed" })
      )
      base = commit!(repo, "merge newer failing evidence")
      write_file(repo, "docs/README.md", status_projection_document(status: "Unsupported stable claim."))
      commit!(repo, "project status from superseded evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: passed_path,
        current_docs: "docs/README.md status projection only"
      )
      expect_failure(repo, body, base, "requires latest Passed, unwaived evidence")
    end
  end,
  "D4 status projection rejects evidence modified before the PR base" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(repo)
      write_file(repo, "docs/README.md", status_projection_document(status: "Unverified status."))
      target = commit!(repo, "freeze status projection markers")
      record_path = evidence_path(target, scope: "historically-modified")
      write_file(repo, record_path, evidence_document(target: target))
      commit!(repo, "merge passing evidence")
      modified = evidence_document(target: target).sub("observed outcome", "rewritten outcome")
      write_file(repo, record_path, modified)
      base = commit!(repo, "rewrite merged evidence before projection")
      write_file(repo, "docs/README.md", status_projection_document(status: "Unsupported stable claim."))
      commit!(repo, "project status from rewritten evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "docs/README.md status projection only"
      )
      expect_failure(repo, body, base, "immutable evidence record changed after first-parent addition")
    end
  end,
  "D4 status projection rejects a historically deleted newer result" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(repo)
      write_file(repo, "docs/README.md", status_projection_document(status: "Unverified status."))
      target = commit!(repo, "freeze status projection markers")
      passed_path = evidence_path(target, scope: "surviving-pass")
      write_file(repo, passed_path, evidence_document(target: target))
      commit!(repo, "merge older passing evidence")
      failed_path = evidence_path(target, scope: "deleted-failure")
      write_file(
        repo,
        failed_path,
        evidence_document(target: target, overall: "Failed", gates: { GATE_ID => "Failed" })
      )
      commit!(repo, "merge newer failing evidence")
      FileUtils.rm(File.join(repo, failed_path))
      base = commit!(repo, "delete newer failing evidence before projection")
      write_file(repo, "docs/README.md", status_projection_document(status: "Unsupported stable claim."))
      commit!(repo, "project status after deleting failure")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: passed_path,
        current_docs: "docs/README.md status projection only"
      )
      expect_failure(repo, body, base, "immutable evidence record was deleted after first-parent addition")
    end
  end,
  "D4 status projection rejects evidence for only part of the frozen Gate set" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(
        repo,
        gate_ids: [GATE_ID, SECOND_GATE_ID]
      )
      write_file(repo, "docs/README.md", status_projection_document(status: "Unverified status."))
      target = commit!(repo, "freeze status projection markers")
      record_path = evidence_path(target, scope: "partial-gate-set")
      write_file(repo, record_path, evidence_document(target: target))
      base = commit!(repo, "merge evidence for only one frozen Gate")
      write_file(repo, "docs/README.md", status_projection_document(status: "Unsupported stable claim."))
      commit!(repo, "project status from incomplete Gate evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "docs/README.md status projection only"
      )
      expect_failure(repo, body, base, "must cover every frozen Gate")
    end
  end,
  "D4 status projection with root README still requires merged evidence" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(repo)
      write_file(
        repo,
        "docs/README.md",
        status_projection_document(status: "Unverified status.")
      )
      base = commit!(repo, "freeze status projection markers")
      write_file(
        repo,
        "docs/README.md",
        status_projection_document(status: "Unsupported stable claim.")
      )
      write_file(repo, "README.md", "# Unsupported root stable claim\n")
      commit!(repo, "project status without evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}"
      )
      expect_failure(
        repo,
        body,
        base,
        "D4 status-projection Implementation requires at least one merged Evidence record"
      )
    end
  end,
  "D4 may add projection markers before evidence exists" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, approved_base = approved_fixture(repo)
      write_file(
        repo,
        "docs/README.md",
        status_projection_document(status: "Unverified status.")
      )
      commit!(repo, "add projection markers before evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}"
      )
      expect_success(repo, body, approved_base)
    end
  end,
  "D4 may change projection-adjacent contract text before rerunning evidence" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(repo)
      write_file(
        repo,
        "docs/README.md",
        status_projection_document(status: "Unverified status.")
      )
      base = commit!(repo, "freeze current documentation contract")
      write_file(
        repo,
        "docs/README.md",
        status_projection_document(
          status: "Unverified status.",
          contract: "Changed contract requiring new evidence."
        )
      )
      commit!(repo, "change contract before rerunning evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}"
      )
      expect_success(repo, body, base)
    end
  end,
  "D4 runtime Implementation may remain pre-evidence" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, approved_base = approved_fixture(repo)
      write_file(repo, "src/lib.rs", "pub fn d4_runtime_change() {}\n")
      commit!(repo, "implement D4 runtime before evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}"
      )
      expect_success(repo, body, approved_base)
    end
  end,
  "D4 Implementation rejects evidence added in the same PR" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(repo)
      write_file(
        repo,
        "docs/README.md",
        status_projection_document(status: "Unverified status.")
      )
      base = commit!(repo, "freeze status projection markers")
      record_path = evidence_path(base, scope: "unmerged")
      write_file(repo, record_path, evidence_document(target: base))
      write_file(
        repo,
        "docs/README.md",
        status_projection_document(status: "Premature status projection.")
      )
      commit!(repo, "add evidence and projection together")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path
      )
      expect_failure(
        repo,
        body,
        base,
        "new verification records require an Evidence-only PR"
      )
    end
  end,
  "D4 Implementation rejects rewriting merged evidence" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(repo)
      write_file(
        repo,
        "docs/README.md",
        status_projection_document(status: "Unverified status.")
      )
      target = commit!(repo, "freeze status projection markers")
      record_path = evidence_path(target, scope: "immutable")
      write_file(repo, record_path, evidence_document(target: target))
      base = commit!(repo, "merge immutable evidence before projection")
      write_file(
        repo,
        record_path,
        evidence_document(
          target: target,
          overall: "Failed",
          gates: { GATE_ID => "Failed" }
        )
      )
      write_file(
        repo,
        "docs/README.md",
        status_projection_document(status: "Projection with rewritten evidence.")
      )
      commit!(repo, "rewrite evidence during projection")
      body = pr_body(
        change_class: "D4",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path
      )
      expect_failure(repo, body, base, "verification records are immutable")
    end
  end,
  "D3 Implementation rejects evidence declarations" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, approved_base = approved_fixture(repo)
      record_path = evidence_path(approved_base, scope: "d3")
      write_file(repo, record_path, evidence_document(target: approved_base))
      base = commit!(repo, "merge evidence before D3 implementation")
      write_file(repo, "src/lib.rs", "pub fn d3_runtime_change() {}\n")
      commit!(repo, "declare unrelated evidence in D3")
      body = pr_body(
        change_class: "D3",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path
      )
      expect_failure(repo, body, base, "Implementation requires 'Evidence record(s): Not applicable")
    end
  end,
  "Implementation rejects verification and release promotion without Evidence-only" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, approval, approved_base = approved_fixture(repo)
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Approved",
          approval: approval,
          adr_status: "Accepted",
          delivery: "Implemented",
          verification: "Verified",
          release: "Stable"
        )
      )
      write_file(repo, "src/lib.rs", "pub fn implementation() {}\n")
      commit!(repo, "implement and claim verification")
      body = pr_body(
        change_class: "D3",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}"
      )
      expect_failure(repo, body, approved_base, "Implementation may not change 验证状态")
    end
  end,
  "Evidence-only accepts immutable Passed evidence for every frozen Gate" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, approval, target = implemented_fixture(repo)
      record_path = evidence_path(target)
      write_file(repo, record_path, evidence_document(target: target))
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Approved",
          approval: approval,
          adr_status: "Accepted",
          delivery: "Implemented",
          verification: "Verified",
          release: "Stable"
        )
      )
      commit!(repo, "record evidence and advance lifecycle")
      body = pr_body(
        change_class: "D4",
        purpose: "Evidence-only",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "No Current contract documents; design lifecycle metadata only"
      )
      expect_success(repo, body, target)
    end
  end,
  "Evidence-only rejects a mutable artifact reference without SHA-256" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, target = implemented_fixture(repo)
      record_path = evidence_path(target)
      invalid = evidence_document(target: target).sub(/sha256:[a-f0-9]{64}/, "latest")
      write_file(repo, record_path, invalid)
      commit!(repo, "record mutable artifact reference")
      body = pr_body(
        change_class: "D4",
        purpose: "Evidence-only",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "No Current contract documents; evidence only"
      )
      expect_failure(repo, body, target, "Artifact / hash must contain sha256:<64 lowercase hex>")
    end
  end,
  "Evidence-only rejects a Gate table hidden in an HTML comment" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, target = implemented_fixture(repo)
      record_path = evidence_path(target)
      hidden = evidence_document(target: target)
        .sub("| Gate ID", "<!--\n| Gate ID")
        .sub(/(sha256:[a-f0-9]{64} \|)/, "\\1\n-->")
      write_file(repo, record_path, hidden)
      commit!(repo, "hide evidence table")
      body = pr_body(
        change_class: "D4",
        purpose: "Evidence-only",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "No Current contract documents; evidence only"
      )
      expect_failure(repo, body, target, "may not hide evidence in HTML")
    end
  end,
  "Evidence-only rejects Verified when a frozen Gate lacks Passed evidence" => lambda do
    with_repo do |repo, _initial|
      design_body = "Frozen behavior also requires `#{SECOND_GATE_ID}`."
      write_file(repo, ADR_PATH, adr_document(status: "Accepted"))
      write_file(repo, DESIGN_PATH, design_document(adr_status: "Accepted", body: design_body))
      frozen_sha = commit!(repo, "freeze design with two Gates")
      approval = "reviewer@example.invalid + approval commit #{frozen_sha}"
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Approved",
          approval: approval,
          adr_status: "Accepted",
          body: design_body
        )
      )
      approved_base = commit!(repo, "approve two-Gate design")
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Approved",
          approval: approval,
          adr_status: "Accepted",
          delivery: "Implemented",
          body: design_body
        )
      )
      write_file(repo, "src/lib.rs", "pub fn implemented_behavior() {}\n")
      target = commit!(repo, "implement two-Gate design")
      raise "approval commit should precede target" if approved_base == target

      record_path = evidence_path(target)
      write_file(repo, record_path, evidence_document(target: target))
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Approved",
          approval: approval,
          adr_status: "Accepted",
          delivery: "Implemented",
          verification: "Verified",
          body: design_body
        )
      )
      commit!(repo, "claim Verified with one missing Gate")
      body = pr_body(
        change_class: "D4",
        purpose: "Evidence-only",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "No Current contract documents; design lifecycle metadata only"
      )
      expect_failure(repo, body, target, "Verified requires Passed evidence for every frozen Gate")
    end
  end,
  "Evidence-only uses the latest first-parent Gate result instead of any historical Passed result" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, approval, target = implemented_fixture(repo)
      old_record = evidence_path(target, scope: "old-pass")
      write_file(repo, old_record, evidence_document(target: target))
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Approved",
          approval: approval,
          adr_status: "Accepted",
          delivery: "Implemented",
          verification: "Verified",
          release: "Stable"
        )
      )
      base = commit!(repo, "merge earlier Passed evidence")

      new_record = evidence_path(target, scope: "new-failure")
      write_file(
        repo,
        new_record,
        evidence_document(target: target, overall: "Failed", gates: { GATE_ID => "Failed" })
      )
      commit!(repo, "record later Failed evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Evidence-only",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: new_record,
        current_docs: "No Current contract documents; design lifecycle metadata only"
      )
      expect_failure(repo, body, base, "Verified requires Passed evidence for every frozen Gate")
    end
  end,
  "Evidence-only permits a lifecycle downgrade required by newer Failed evidence" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, approval, target = implemented_fixture(repo)
      old_record = evidence_path(target, scope: "old-pass")
      write_file(repo, old_record, evidence_document(target: target))
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Approved",
          approval: approval,
          adr_status: "Accepted",
          delivery: "Implemented",
          verification: "Verified",
          release: "Stable"
        )
      )
      base = commit!(repo, "merge earlier stable evidence")

      new_record = evidence_path(target, scope: "new-failure")
      write_file(
        repo,
        new_record,
        evidence_document(target: target, overall: "Failed", gates: { GATE_ID => "Failed" })
      )
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Approved",
          approval: approval,
          adr_status: "Accepted",
          delivery: "Implemented",
          verification: "Unverified",
          release: "Prerelease"
        )
      )
      commit!(repo, "record failure and downgrade lifecycle")
      body = pr_body(
        change_class: "D4",
        purpose: "Evidence-only",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: new_record,
        current_docs: "No Current contract documents; design lifecycle metadata only"
      )
      expect_success(repo, body, base)
    end
  end,
  "Evidence-only does not count a waived Passed result toward Verified" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, approval, target = implemented_fixture(repo)
      record_path = evidence_path(target)
      write_file(
        repo,
        record_path,
        evidence_document(target: target, waiver: "issue-1234 accepted limitation")
      )
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Approved",
          approval: approval,
          adr_status: "Accepted",
          delivery: "Implemented",
          verification: "Verified"
        )
      )
      commit!(repo, "claim Verified with waived evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Evidence-only",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "No Current contract documents; design lifecycle metadata only"
      )
      expect_failure(repo, body, target, "Verified requires Passed evidence for every frozen Gate")
    end
  end,
  "Evidence-only rejects duplicate Gate IDs across new records" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, target = implemented_fixture(repo)
      first_record = evidence_path(target, scope: "first")
      second_record = evidence_path(target, scope: "second")
      write_file(repo, first_record, evidence_document(target: target))
      write_file(repo, second_record, evidence_document(target: target))
      commit!(repo, "duplicate one Gate across two records")
      body = pr_body(
        change_class: "D4",
        purpose: "Evidence-only",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: "#{first_record}, #{second_record}",
        current_docs: "No Current contract documents; evidence only"
      )
      expect_failure(repo, body, target, "may not record the same Gate ID more than once")
    end
  end,
  "Evidence-only rejects an evidence target created inside the same PR" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, base = implemented_fixture(repo)
      write_file(repo, "docs/verification/README.md", "temporary in-PR commit marker\n")
      in_pr_target = commit!(repo, "create in-PR evidence target")
      record_path = evidence_path(in_pr_target)
      FileUtils.rm(File.join(repo, "docs/verification/README.md"))
      write_file(repo, record_path, evidence_document(target: in_pr_target))
      commit!(repo, "record evidence for in-PR target")
      body = pr_body(
        change_class: "D4",
        purpose: "Evidence-only",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "No Current contract documents; evidence only"
      )
      expect_failure(repo, body, base, "evidence target must be a commit that is an ancestor of PR base")
    end
  end,
  "Evidence-only rejects Current contract body changes" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, implementation = implemented_fixture(repo)
      current_contract = <<~MARKDOWN
        # Current protocol

        - 文档适用性：Current
        - 适用范围：fixture protocol
        - 设计评审状态：N/A
        - ADR 决策状态：N/A
        - 交付状态：Implemented
        - 验证状态：Unverified
        - 发布状态：Unreleased

        Frozen current behavior.
      MARKDOWN
      write_file(repo, "docs/protocol.md", current_contract)
      target = commit!(repo, "add current contract before evidence")
      record_path = evidence_path(target)
      write_file(repo, record_path, evidence_document(target: target))
      write_file(repo, "docs/protocol.md", current_contract.sub("Frozen", "Rewritten"))
      commit!(repo, "mix contract rewrite with evidence")
      body = pr_body(
        change_class: "D4",
        purpose: "Evidence-only",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}",
        evidence: record_path,
        current_docs: "docs/protocol.md"
      )
      expect_failure(repo, body, target, "Evidence-only may change only verification/release lifecycle metadata")
      raise "implementation fixture unexpectedly changed" if implementation == target
    end
  end,
  "Implementation rejects approval metadata introduced in the same PR" => lambda do
    with_repo do |repo, _initial|
      write_file(repo, ADR_PATH, adr_document(status: "Accepted"))
      write_file(repo, DESIGN_PATH, design_document(adr_status: "Accepted"))
      base = commit!(repo, "freeze draft on PR base")
      approval = "reviewer@example.invalid + approval commit #{base}"
      write_file(
        repo,
        DESIGN_PATH,
        design_document(review: "Approved", approval: approval, adr_status: "Accepted")
      )
      write_file(repo, "src/lib.rs", "pub fn implementation() {}\n")
      commit!(repo, "approve and implement together")
      body = pr_body(
        change_class: "D3",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{base}"
      )
      expect_failure(repo, body, base, "PR base must already contain the Approved design")
    end
  end,
  "Design-only rejects delivery claims on a new Draft" => lambda do
    with_repo do |repo, base|
      write_file(
        repo,
        DESIGN_PATH,
        design_document(delivery: "Implemented", verification: "Verified", release: "Stable")
      )
      commit!(repo, "claim delivery in new draft")
      body = pr_body(change_class: "D2", purpose: "Design-only")
      expect_failure(repo, body, base, "new Design-only record must use 交付状态: Not started")
    end
  end,
  "Design-only rejects verification upgrades on an existing Draft" => lambda do
    with_repo do |repo, _initial|
      write_file(repo, DESIGN_PATH, design_document)
      base = commit!(repo, "merge unverified draft")
      write_file(repo, DESIGN_PATH, design_document(verification: "Verified"))
      commit!(repo, "claim verification in design review")
      body = pr_body(change_class: "D2", purpose: "Design-only")
      expect_failure(repo, body, base, "Design-only may not change 验证状态 from Unverified to Verified")
    end
  end,
  "ADR-only Design-only rejects release claims on a new ADR" => lambda do
    with_repo do |repo, base|
      write_file(repo, ADR_PATH, adr_document(release: "Stable"))
      commit!(repo, "claim release in ADR proposal")
      body = pr_body(
        change_class: "D3",
        purpose: "Design-only",
        design: "Not applicable: standalone ADR proposal",
        adr: ADR_PATH,
        review: "Not applicable: no detailed design in this PR",
        gates: "Not applicable: gates belong to the later detailed design"
      )
      expect_failure(repo, body, base, "new Design-only record must use 发布状态: Unreleased")
    end
  end,
  "Design-only rejects duplicate lifecycle metadata" => lambda do
    with_repo do |repo, base|
      duplicate = design_document.sub(
        "- 验证状态：Unverified",
        "- 交付状态：Implemented\n- 验证状态：Unverified"
      )
      write_file(repo, DESIGN_PATH, duplicate)
      commit!(repo, "add ambiguous draft metadata")
      body = pr_body(change_class: "D2", purpose: "Design-only")
      expect_failure(repo, body, base, "duplicate 交付状态 metadata near the top")
    end
  end,
  "Design-only rejects metadata hidden in a document comment" => lambda do
    with_repo do |repo, base|
      hidden = design_document
        .sub("- 文档适用性", "<!--\n- 文档适用性")
        .sub("- 发布状态：Unreleased", "- 发布状态：Unreleased\n-->")
      write_file(repo, DESIGN_PATH, hidden)
      commit!(repo, "hide design metadata")
      body = pr_body(change_class: "D2", purpose: "Design-only")
      expect_failure(repo, body, base, "metadata must be a visible blockquote or list")
    end
  end,
  "Design-only rejects metadata hidden by raw HTML" => lambda do
    with_repo do |repo, base|
      hidden = design_document.sub(
        "- 文档适用性：Proposed",
        "- <span hidden>文档适用性：Proposed</span>"
      )
      write_file(repo, DESIGN_PATH, hidden)
      commit!(repo, "hide design metadata with HTML")
      body = pr_body(change_class: "D2", purpose: "Design-only")
      expect_failure(repo, body, base, "metadata block may not contain raw HTML")
    end
  end,
  "Design-only rejects source changes" => lambda do
    with_repo do |repo, base|
      write_file(repo, DESIGN_PATH, design_document)
      write_file(repo, "src/lib.rs", "pub fn changed() {}\n")
      commit!(repo, "mix source into proposal")
      body = pr_body(change_class: "D2", purpose: "Design-only")
      expect_failure(repo, body, base, "Design-only contains forbidden changes: src/lib.rs")
    end
  end,
  "Design-only rejects an undeclared Current contract change" => lambda do
    with_repo do |repo, _initial|
      write_file(repo, "docs/protocol.md", "# Current protocol\n")
      base = commit!(repo, "add current protocol")
      write_file(repo, DESIGN_PATH, design_document)
      write_file(repo, "docs/protocol.md", "# Rewritten current protocol\n")
      commit!(repo, "mix current contract into proposal")
      body = pr_body(change_class: "D2", purpose: "Design-only")
      expect_failure(repo, body, base, "Design-only contains forbidden changes: docs/protocol.md")
    end
  end,
  "Design-only rejects a Current document disguised as a proposal" => lambda do
    with_repo do |repo, _initial|
      write_file(
        repo,
        "docs/protocol.md",
        design_document(review: "Approved", applicability: "Current")
      )
      base = commit!(repo, "add current contract")
      write_file(repo, "docs/protocol.md", design_document)
      commit!(repo, "disguise current contract")
      body = pr_body(
        change_class: "D2",
        purpose: "Design-only",
        design: "docs/protocol.md"
      )
      expect_failure(repo, body, base, "may not downgrade a Current base document")
    end
  end,
  "Design-only rejects an Approved design downgrade" => lambda do
    with_repo do |repo, _initial|
      write_file(
        repo,
        DESIGN_PATH,
        design_document(review: "Approved", approval: "historical approval")
      )
      base = commit!(repo, "add approved design")
      write_file(repo, DESIGN_PATH, design_document)
      commit!(repo, "downgrade approval")
      body = pr_body(change_class: "D2", purpose: "Design-only")
      expect_failure(repo, body, base, "invalid design review transition Approved -> Draft")
    end
  end,
  "Design-only rejects finalized approval metadata changes" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, base = approved_fixture(repo)
      changed_approval = "different-reviewer@example.invalid + approval commit #{frozen_sha}"
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Approved",
          approval: changed_approval,
          adr_status: "Accepted",
          delivery: "In progress"
        )
      )
      commit!(repo, "rewrite approval record")
      body = pr_body(
        change_class: "D3",
        purpose: "Design-only",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}"
      )
      expect_failure(repo, body, base, "finalized design body or immutable metadata may not be rewritten")
    end
  end,
  "checker rejects fields hidden in HTML comments" => lambda do
    with_repo do |repo, base|
      visible = pr_body(
        change_class: "D3",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Draft; approval pending"
      )
      hidden = <<~FIELDS
        <!--
        - Change class: D0
        - PR purpose: Implementation
        - Proposed design: Not applicable: hidden bypass
        - Required ADR(s): Not applicable: hidden bypass
        - Design review status and approval commit: Not applicable: hidden bypass
        - Atomic Gate ID(s): Not applicable: hidden bypass
        -->
      FIELDS
      body = visible.sub("## Design contract\n\n", "## Design contract\n\n#{hidden}")
      expect_failure(repo, body, base, "Pull Request body may not contain HTML comments")
    end
  end,
  "checker rejects duplicate Design contract fields" => lambda do
    with_repo do |repo, base|
      write_file(repo, DESIGN_PATH, design_document)
      commit!(repo, "add draft for duplicate field test")
      body = pr_body(change_class: "D2", purpose: "Design-only")
      body = body.sub("- Change class: D2", "- Change class: D0\n- Change class: D2")
      expect_failure(repo, body, base, "exactly one non-empty field 'Change class'; found 2")
    end
  end,
  "Implementation rejects approved design body drift" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, approval, approved_base = approved_fixture(repo)
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Approved",
          approval: approval,
          adr_status: "Accepted",
          delivery: "In progress",
          body: "Changed without a new approval."
        )
      )
      commit!(repo, "rewrite approved design")
      body = pr_body(
        change_class: "D3",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}"
      )
      expect_failure(repo, body, approved_base, "Approved design body or immutable metadata changed")
    end
  end,
  "Implementation rejects approval record changes" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, approved_base = approved_fixture(repo)
      changed_approval = "different-reviewer@example.invalid + approval commit #{frozen_sha}"
      write_file(
        repo,
        DESIGN_PATH,
        design_document(
          review: "Approved",
          approval: changed_approval,
          adr_status: "Accepted",
          delivery: "In progress"
        )
      )
      commit!(repo, "rewrite approval during implementation")
      body = pr_body(
        change_class: "D3",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}"
      )
      expect_failure(repo, body, approved_base, "Implementation may not rewrite the design approval record")
    end
  end,
  "Implementation rejects Accepted ADR body drift" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, approved_base = approved_fixture(repo)
      write_file(
        repo,
        ADR_PATH,
        adr_document(status: "Accepted", delivery: "Implemented", body: "Rewritten decision.")
      )
      commit!(repo, "rewrite accepted ADR")
      body = pr_body(
        change_class: "D3",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}"
      )
      expect_failure(repo, body, approved_base, "Accepted ADR body or immutable metadata changed")
    end
  end,
  "Implementation rejects an ADR altered on the PR base then restored" => lambda do
    with_repo do |repo, _initial|
      frozen_sha, _approval, _approved_base = approved_fixture(repo)
      write_file(
        repo,
        ADR_PATH,
        adr_document(status: "Accepted", delivery: "Implemented", body: "Drifted on base.")
      )
      base = commit!(repo, "rewrite accepted ADR before implementation")
      write_file(repo, ADR_PATH, adr_document(status: "Accepted", delivery: "Implemented"))
      commit!(repo, "restore ADR inside implementation")
      body = pr_body(
        change_class: "D3",
        purpose: "Implementation",
        adr: ADR_PATH,
        review: "Approved; #{frozen_sha}"
      )
      expect_failure(repo, body, base, "Accepted ADR at PR base does not match its frozen snapshot")
    end
  end
}.freeze

failures = []
tests.each_with_index do |(name, test), index|
  test.call
  puts "ok #{index + 1} - #{name}"
rescue StandardError => error
  failures << "not ok #{index + 1} - #{name}: #{error.message}"
  warn failures.last
end

abort "#{failures.length} PR contract test(s) failed" unless failures.empty?
puts "validated #{tests.length} PR contract scenarios"
