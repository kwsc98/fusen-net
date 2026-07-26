#!/usr/bin/env ruby
# SPDX-License-Identifier: Apache-2.0 OR MIT

require "fileutils"
require "open3"
require "rbconfig"
require "tmpdir"

CHECKER = File.expand_path("check-release-evidence.rb", __dir__)
GATES = %w[TEST-RELEASE-CODE-01 TEST-RELEASE-NETWORK-01].freeze
ARTIFACT = "sha256:#{'a' * 64}"

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
  Dir.mktmpdir("stellaris-release-evidence-") do |repo|
    git!(repo, "init", "-q")
    git!(repo, "config", "user.name", "Release Evidence Test")
    git!(repo, "config", "user.email", "release-evidence@example.invalid")
    write_file(repo, "src/lib.rs", "pub fn runtime() {}\n")
    write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Unverified", release: "Unreleased"))
    target = commit!(repo, "runtime target")
    yield repo, target
  end
end

def release_plan(verification:, release:)
  gates = GATES.map { |gate| "| `#{gate}` | Required |" }.join("\n")
  <<~MARKDOWN
    # Release plan

    - 文档适用性：Current
    - 适用范围：release fixture
    - 设计评审状态：N/A
    - ADR 决策状态：N/A
    - 交付状态：Implemented
    - 验证状态：#{verification}
    - 发布状态：#{release}

    | Gate ID | Requirement |
    | --- | --- |
    #{gates}
  MARKDOWN
end

def current_implementation_document(verification:, release:)
  <<~MARKDOWN
    # Runtime status

    - 文档适用性：Current
    - 适用范围：release fixture runtime
    - 设计评审状态：N/A
    - ADR 决策状态：N/A
    - 交付状态：Implemented
    - 验证状态：#{verification}
    - 发布状态：#{release}
  MARKDOWN
end

def support_projection(status:, contract: "IPv4-only", protocol_status: "Protocol unverified.")
  <<~MARKDOWN
    # Compatibility

    - 文档适用性：Current
    - 适用范围：release fixture support
    - 设计评审状态：N/A
    - ADR 决策状态：N/A
    - 交付状态：N/A
    - 验证状态：N/A
    - 发布状态：N/A

    ## Release qualification

    <!-- stellaris-release-status:support-status:start -->
    #{status}
    <!-- stellaris-release-status:support-status:end -->

    ## Protocol qualification

    <!-- stellaris-release-status:protocol-status:start -->
    #{protocol_status}
    <!-- stellaris-release-status:protocol-status:end -->

    ## Transport qualification

    <!-- stellaris-release-status:transport-status:start -->
    Transport unverified.
    <!-- stellaris-release-status:transport-status:end -->

    ## Platform qualification

    <!-- stellaris-release-status:platform-status:start -->
    Platforms unverified.
    <!-- stellaris-release-status:platform-status:end -->

    ## Frozen contract

    Runtime boundary: #{contract}.
  MARKDOWN
end

def evidence_record(
  target,
  gates: GATES,
  artifact: ARTIFACT,
  overall: "Passed",
  result: "Passed",
  waiver: "None",
  separator: "| --- | --- | --- | --- | --- | --- |",
  command: "fixture command"
)
  rows = gates.map do |gate|
    "| `#{gate}` | #{command} | observable result | observed | #{result} | #{artifact} |"
  end.join("\n")
  <<~MARKDOWN
    # Release evidence

    - 文档适用性：Current
    - 适用范围：release fixture evidence
    - 设计评审状态：N/A
    - ADR 决策状态：N/A
    - 交付状态：N/A
    - 验证状态：N/A
    - 发布状态：N/A
    - 证据目标：#{target}
    - 总体结果：#{overall}
    - 豁免：#{waiver}

    | Gate ID | 命令或场景 | 预期 | 实际 | 结果 | Artifact / hash |
    #{separator}
    #{rows}
  MARKDOWN
end

def evidence_path(target, scope = "fixture")
  "docs/verification/2026-07-26-#{scope}-#{target[0, 7]}.md"
end

def run_checker(repo, release_commit, stable: "true")
  run_command(
    RbConfig.ruby,
    CHECKER,
    chdir: repo,
    env: { "IS_STABLE" => stable, "RELEASE_COMMIT" => release_commit }
  )
end

def expect_success(repo, release_commit, stable: "true")
  stdout, stderr, status = run_checker(repo, release_commit, stable: stable)
  return if status.success?
  raise "expected success, got #{status.exitstatus}: #{stderr}#{stdout}"
end

def expect_failure(repo, release_commit, message)
  stdout, stderr, status = run_checker(repo, release_commit)
  raise "expected failure, checker succeeded: #{stdout}" if status.success?
  output = "#{stderr}#{stdout}"
  raise "expected #{message.inspect}, got #{output.inspect}" unless output.include?(message)
end

tests = {
  "prerelease does not claim stable qualification" => lambda do
    with_repo do |repo, target|
      expect_success(repo, target, stable: "false")
    end
  end,
  "stable release requires Verified plan metadata" => lambda do
    with_repo do |repo, target|
      expect_failure(repo, target, "stable release plan must be Verified")
    end
  end,
  "stable release reads plan metadata from the release commit, not a dirty worktree" => lambda do
    with_repo do |repo, target|
      write_file(repo, evidence_path(target), evidence_record(target))
      release_commit = commit!(repo, "record evidence without promoting the plan")
      write_file(
        repo,
        "docs/distributed-network-plan.md",
        release_plan(verification: "Verified", release: "Stable")
      )
      expect_failure(repo, release_commit, "stable release plan must be Verified")
    end
  end,
  "stable release reads Current document status from the release commit" => lambda do
    with_repo do |repo, _initial_target|
      status_path = "docs/runtime-status.md"
      write_file(
        repo,
        status_path,
        current_implementation_document(verification: "Unverified", release: "Unreleased")
      )
      target = commit!(repo, "freeze an unverified Current document")
      write_file(
        repo,
        "docs/distributed-network-plan.md",
        release_plan(verification: "Verified", release: "Stable")
      )
      write_file(repo, evidence_path(target), evidence_record(target))
      release_commit = commit!(repo, "record evidence without promoting the Current document")
      write_file(
        repo,
        status_path,
        current_implementation_document(verification: "Verified", release: "Stable")
      )
      expect_failure(repo, release_commit, "#{status_path} (Unverified / Unreleased)")
    end
  end,
  "stable release accepts complete evidence after docs-only changes" => lambda do
    with_repo do |repo, target|
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      write_file(repo, evidence_path(target), evidence_record(target))
      release_commit = commit!(repo, "record complete evidence")
      expect_success(repo, release_commit)
    end
  end,
  "stable release rejects missing Gate evidence" => lambda do
    with_repo do |repo, target|
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      write_file(repo, evidence_path(target), evidence_record(target, gates: [GATES.first]))
      release_commit = commit!(repo, "record partial evidence")
      expect_failure(repo, release_commit, GATES.last)
    end
  end,
  "newer Failed evidence overrides an older Passed result by first-parent order" => lambda do
    with_repo do |repo, target|
      write_file(repo, evidence_path(target, "z-older"), evidence_record(target))
      commit!(repo, "record an older passing result")
      write_file(
        repo,
        evidence_path(target, "a-newer"),
        evidence_record(target, overall: "Failed", result: "Failed")
      )
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      release_commit = commit!(repo, "record a newer failing result")
      expect_failure(repo, release_commit, "missing Passed evidence")
    end
  end,
  "newer Passed evidence recovers from an older Failed result by first-parent order" => lambda do
    with_repo do |repo, target|
      write_file(
        repo,
        evidence_path(target, "z-older"),
        evidence_record(target, overall: "Failed", result: "Failed")
      )
      commit!(repo, "record an older failing result")
      write_file(repo, evidence_path(target, "a-newer"), evidence_record(target))
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      release_commit = commit!(repo, "record a newer passing result")
      expect_success(repo, release_commit)
    end
  end,
  "latest Passed evidence with a waiver does not qualify Stable" => lambda do
    with_repo do |repo, target|
      write_file(repo, evidence_path(target, "z-older"), evidence_record(target))
      commit!(repo, "record an older unwaived result")
      write_file(
        repo,
        evidence_path(target, "a-newer"),
        evidence_record(target, waiver: "issue-123 accepted limitation")
      )
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      release_commit = commit!(repo, "record a newer waived result")
      expect_failure(repo, release_commit, "missing Passed evidence")
    end
  end,
  "stable release rejects stale evidence after source changes" => lambda do
    with_repo do |repo, target|
      write_file(repo, "src/lib.rs", "pub fn changed_runtime() {}\n")
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      write_file(repo, evidence_path(target), evidence_record(target))
      release_commit = commit!(repo, "change runtime after evidence")
      expect_failure(repo, release_commit, "missing Passed evidence")
    end
  end,
  "stable release rejects evidence after Gate definitions change" => lambda do
    with_repo do |repo, target|
      reduced_plan = release_plan(verification: "Verified", release: "Stable")
        .lines
        .reject { |line| line.include?(GATES.last) }
        .join
      write_file(repo, "docs/distributed-network-plan.md", reduced_plan)
      write_file(repo, evidence_path(target), evidence_record(target, gates: [GATES.first]))
      release_commit = commit!(repo, "remove a Gate after testing")
      expect_failure(repo, release_commit, "missing Passed evidence")
    end
  end,
  "stable release accepts an allowlisted support-status projection" => lambda do
    with_repo do |repo, _initial_target|
      write_file(repo, "docs/compatibility.md", support_projection(status: "Unverified."))
      target = commit!(repo, "freeze support projection boundary")
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      write_file(repo, "docs/compatibility.md", support_projection(status: "Linux stable; other platforms compile-only."))
      write_file(repo, evidence_path(target), evidence_record(target))
      release_commit = commit!(repo, "project qualified support status")
      expect_success(repo, release_commit)
    end
  end,
  "stable release rejects contract changes outside a support-status projection" => lambda do
    with_repo do |repo, _initial_target|
      write_file(repo, "docs/compatibility.md", support_projection(status: "Unverified."))
      target = commit!(repo, "freeze support projection boundary")
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      write_file(
        repo,
        "docs/compatibility.md",
        support_projection(status: "Linux stable.", contract: "IPv4 and IPv6")
      )
      write_file(repo, evidence_path(target), evidence_record(target))
      release_commit = commit!(repo, "change support contract after testing")
      expect_failure(repo, release_commit, "missing Passed evidence")
    end
  end,
  "stable release rejects a support-status marker added after testing" => lambda do
    with_repo do |repo, _initial_target|
      unmarked = support_projection(status: "Unverified.")
        .gsub(/^<!-- stellaris-release-status:support-status:(?:start|end) -->\n/, "")
      write_file(repo, "docs/compatibility.md", unmarked)
      target = commit!(repo, "record unmarked support status")
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      write_file(repo, "docs/compatibility.md", support_projection(status: "Linux stable."))
      write_file(repo, evidence_path(target), evidence_record(target))
      release_commit = commit!(repo, "add projection marker after testing")
      expect_failure(repo, release_commit, "missing Passed evidence")
    end
  end,
  "stable release rejects Gate IDs inside a support-status projection" => lambda do
    with_repo do |repo, _initial_target|
      write_file(repo, "docs/compatibility.md", support_projection(status: "Unverified."))
      target = commit!(repo, "freeze support projection boundary")
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      write_file(repo, "docs/compatibility.md", support_projection(status: "`TEST-RELEASE-CODE-01` passed."))
      write_file(repo, evidence_path(target), evidence_record(target))
      release_commit = commit!(repo, "put a Gate in release projection")
      expect_failure(repo, release_commit, "may not contain Gate IDs")
    end
  end,
  "stable release rejects duplicate support-status projections" => lambda do
    with_repo do |repo, _initial_target|
      write_file(repo, "docs/compatibility.md", support_projection(status: "Unverified."))
      target = commit!(repo, "freeze support projection boundary")
      duplicate = support_projection(status: "Linux stable.") + <<~MARKDOWN

        <!-- stellaris-release-status:support-status:start -->
        Duplicate status.
        <!-- stellaris-release-status:support-status:end -->
      MARKDOWN
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      write_file(repo, "docs/compatibility.md", duplicate)
      write_file(repo, evidence_path(target), evidence_record(target))
      release_commit = commit!(repo, "duplicate support projection")
      expect_failure(repo, release_commit, "duplicate release-status projection support-status")
    end
  end,
  "stable release rejects an unpaired support-status projection" => lambda do
    with_repo do |repo, _initial_target|
      write_file(repo, "docs/compatibility.md", support_projection(status: "Unverified."))
      target = commit!(repo, "freeze support projection boundary")
      unpaired = support_projection(status: "Linux stable.")
        .sub("<!-- stellaris-release-status:support-status:end -->\n", "")
        .split("## Protocol qualification", 2)
        .first
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      write_file(repo, "docs/compatibility.md", unpaired)
      write_file(repo, evidence_path(target), evidence_record(target))
      release_commit = commit!(repo, "remove support projection end marker")
      expect_failure(repo, release_commit, "missing release-status end marker for support-status")
    end
  end,
  "stable release rejects an evidence record modified after addition" => lambda do
    with_repo do |repo, target|
      path = evidence_path(target, "modified")
      write_file(repo, path, evidence_record(target, overall: "Failed", result: "Failed"))
      commit!(repo, "add an immutable failing record")
      write_file(repo, path, evidence_record(target))
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      release_commit = commit!(repo, "rewrite immutable evidence as passing")
      expect_failure(repo, release_commit, "changed after first-parent addition")
    end
  end,
  "stable release rejects deletion of a newer Failed record" => lambda do
    with_repo do |repo, target|
      write_file(repo, evidence_path(target, "z-older"), evidence_record(target))
      commit!(repo, "add an older passing record")
      failed_path = evidence_path(target, "a-newer")
      write_file(repo, failed_path, evidence_record(target, overall: "Failed", result: "Failed"))
      commit!(repo, "add a newer failing record")
      FileUtils.rm(File.join(repo, failed_path))
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      release_commit = commit!(repo, "delete the newer failure")
      expect_failure(repo, release_commit, "deleted after first-parent addition")
    end
  end,
  "stable release rejects a noncanonical evidence filename" => lambda do
    with_repo do |repo, target|
      write_file(repo, "docs/verification/fixture.md", evidence_record(target))
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      release_commit = commit!(repo, "add a noncanonical record")
      expect_failure(repo, release_commit, "invalid immutable verification record filename")
    end
  end,
  "stable release rejects a filename suffix unrelated to the evidence target" => lambda do
    with_repo do |repo, target|
      path = "docs/verification/2026-07-26-wrong-deadbee.md"
      write_file(repo, path, evidence_record(target))
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      release_commit = commit!(repo, "add a misbound evidence filename")
      expect_failure(repo, release_commit, "filename suffix must match")
    end
  end,
  "stable release rejects an inconsistent overall Failed record" => lambda do
    with_repo do |repo, target|
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      write_file(repo, evidence_path(target), evidence_record(target, overall: "Failed"))
      release_commit = commit!(repo, "record failed evidence")
      expect_failure(repo, release_commit, "总体结果 Failed does not match row results Passed")
    end
  end,
  "Gate evidence requires a SHA-256 artifact" => lambda do
    with_repo do |repo, target|
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      write_file(repo, evidence_path(target), evidence_record(target, artifact: "latest"))
      release_commit = commit!(repo, "record placeholder evidence")
      expect_failure(repo, release_commit, "artifact must include sha256:<64 lowercase hex>")
    end
  end,
  "Gate evidence rejects an invalid table separator" => lambda do
    with_repo do |repo, target|
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      write_file(
        repo,
        evidence_path(target),
        evidence_record(target, separator: "| x | x | x | x | x | x |")
      )
      release_commit = commit!(repo, "record malformed table")
      expect_failure(repo, release_commit, "invalid Gate evidence table separator")
    end
  end,
  "Gate evidence rejects an empty command" => lambda do
    with_repo do |repo, target|
      write_file(repo, "docs/distributed-network-plan.md", release_plan(verification: "Verified", release: "Stable"))
      write_file(repo, evidence_path(target), evidence_record(target, command: ""))
      release_commit = commit!(repo, "record empty command")
      expect_failure(repo, release_commit, "has an empty command, expectation, observation, or artifact")
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

abort "#{failures.length} release evidence test(s) failed" unless failures.empty?
puts "validated #{tests.length} release evidence scenarios"
