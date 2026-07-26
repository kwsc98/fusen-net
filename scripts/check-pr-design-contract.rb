#!/usr/bin/env ruby
# SPDX-License-Identifier: Apache-2.0 OR MIT

require "open3"
require "pathname"
require "set"
require_relative "docs_metadata"

DESIGN_ONLY_INDEX_PATHS = Set.new(%w[
  docs/README.md
  docs/design-overview.md
  docs/roadmap.md
  docs/adr/README.md
]).freeze

RELEASE_STATUS_PROJECTIONS = {
  "docs/README.md" => %w[documentation-status],
  "docs/adr/README.md" => %w[adr-status],
  "docs/compatibility.md" => %w[
    support-status
    protocol-status
    transport-status
    platform-status
  ],
  "docs/deployment.md" => %w[deployment-status],
  "docs/design-overview.md" => %w[design-status],
  "docs/releasing.md" => %w[release-status],
  "docs/roadmap.md" => %w[roadmap-status],
  "docs/verification/README.md" => %w[verification-status]
}.freeze
ROOT_RELEASE_STATUS_PROJECTION_PATHS = Set.new(%w[
  README.md
  README.en.md
  CHANGELOG.md
]).freeze
RELEASE_STATUS_MARKER_TOKEN = "stellaris-release-status:"
RELEASE_STATUS_MARKER_PATTERN = /\A<!-- stellaris-release-status:([a-z0-9]+(?:-[a-z0-9]+)*):(start|end) -->\z/

VERIFICATION_RECORD_DIRECTORY = "docs/verification/"
VERIFICATION_RECORD_PATTERN = %r{\Adocs/verification/\d{4}-\d{2}-\d{2}-[A-Za-z0-9._-]+-[0-9a-f]{7,40}\.md\z}
ATOMIC_GATE_ID_PATTERN = /\b[A-Z][A-Z0-9]*(?:-[A-Z0-9]+)+-\d{2}\b/
EVIDENCE_TABLE_HEADER = [
  "Gate ID",
  "命令或场景",
  "预期",
  "实际",
  "结果",
  "Artifact / hash"
].freeze

DESIGN_REVIEW_TRANSITIONS = {
  nil => %w[Draft],
  "Draft" => %w[Draft Approved Rejected Superseded],
  "Approved" => %w[Approved Superseded],
  "Rejected" => %w[Rejected],
  "Superseded" => %w[Superseded]
}.freeze

ADR_DECISION_TRANSITIONS = {
  nil => %w[Proposed],
  "Proposed" => %w[Proposed Accepted Rejected],
  "Accepted" => %w[Accepted Superseded],
  "Rejected" => %w[Rejected],
  "Superseded" => %w[Superseded]
}.freeze

# These fields report lifecycle state; changing them does not change the frozen
# design or decision. Everything else, including Gate definitions, is locked.
DESIGN_MUTABLE_METADATA = %w[
  文档适用性
  设计评审状态
  设计批准
  交付状态
  验证状态
  发布状态
  最后核对
].freeze
ADR_MUTABLE_METADATA = %w[
  文档适用性
  交付状态
  验证状态
  发布状态
  最后核对
].freeze
EVIDENCE_ONLY_MUTABLE_METADATA = %w[
  验证状态
  发布状态
  最后核对
].freeze
LOW_CLASS_LIFECYCLE_METADATA = [
  "文档适用性",
  "设计评审状态",
  "设计批准",
  "关联 ADR",
  "ADR 决策状态",
  "交付状态",
  "验证状态",
  "发布状态"
].freeze
DESIGN_ONLY_MUTABLE_METADATA = [
  "文档适用性",
  "设计评审状态",
  "ADR 决策状态"
].freeze
ADR_DESIGN_ONLY_MUTABLE_METADATA = [
  "文档适用性",
  "ADR 决策状态"
].freeze
DESIGN_ONLY_INITIAL_LIFECYCLE = {
  "交付状态" => "Not started",
  "验证状态" => "Unverified",
  "发布状态" => "Unreleased"
}.freeze

body = ENV.fetch("PR_BODY", "")
abort "Pull Request body is empty; use the repository template" if body.strip.empty?

root_output, root_status = Open3.capture2("git", "rev-parse", "--show-toplevel")
abort "cannot resolve repository root" unless root_status.success?

repo_root = Pathname.new(root_output.strip).expand_path

def git_success?(*arguments)
  system("git", *arguments, out: File::NULL, err: File::NULL)
end

def git_file(commit, path, description: "commit")
  contents, error, status = Open3.capture3("git", "show", "#{commit}:#{path}")
  abort "cannot read #{path} at #{description} #{commit}: #{error.strip}" unless status.success?
  contents
end

def git_file_if_present(commit, path)
  contents, _error, status = Open3.capture3("git", "show", "#{commit}:#{path}")
  status.success? ? contents : nil
end

def changed_paths(base_sha)
  contents, error, status = Open3.capture3(
    "git", "diff", "--name-only", "--no-renames", "-z", "#{base_sha}...HEAD"
  )
  abort "cannot inspect Pull Request changes from #{base_sha}: #{error.strip}" unless status.success?
  contents.split("\0").reject(&:empty?).to_set
end

def diff_paths(from_commit, to_commit)
  contents, error, status = Open3.capture3(
    "git", "diff", "--name-only", "--no-renames", "-z", from_commit, to_commit
  )
  abort "cannot inspect changes from #{from_commit} to #{to_commit}: #{error.strip}" unless status.success?
  contents.split("\0").reject(&:empty?).to_set
end

def pr_base_sha!
  base_sha = ENV.fetch("PR_BASE_SHA", "")
  abort "PR_BASE_SHA from the pull_request event must be a full commit SHA" unless base_sha.match?(/\A[0-9a-f]{40}\z/i)
  abort "PR base SHA is not a commit: #{base_sha}" unless git_success?("cat-file", "-e", "#{base_sha}^{commit}")
  unless git_success?("merge-base", "--is-ancestor", base_sha, "HEAD")
    abort "PR base commit #{base_sha} is not an ancestor of HEAD"
  end
  base_sha
end

def repository_document(repo_root, path)
  candidate = Pathname.new(path)
  abort "document path must be repository-relative: #{path}" if candidate.absolute?
  abort "document path may not traverse a parent directory: #{path}" if candidate.each_filename.include?("..")

  resolved = repo_root.join(candidate).cleanpath
  prefix = "#{repo_root}#{File::SEPARATOR}"
  abort "document path escapes the repository: #{path}" unless resolved.to_s.start_with?(prefix)
  abort "document does not exist: #{path}" unless resolved.file?
  resolved
end

def document_paths(value)
  value.scan(%r{docs/[A-Za-z0-9._/-]+\.md}).uniq
end

def verification_record_paths(value)
  document_paths(value).select { |path| path.start_with?(VERIFICATION_RECORD_DIRECTORY) }
end

def normalize_release_status_projections(contents, path)
  allowed_ids = RELEASE_STATUS_PROJECTIONS[path]
  return nil unless allowed_ids && contents.include?(RELEASE_STATUS_MARKER_TOKEN)

  output = []
  active_id = nil
  block = []
  seen_ids = {}

  contents.lines.each_with_index do |line, index|
    stripped = line.strip
    marker = stripped.match(RELEASE_STATUS_MARKER_PATTERN)
    if stripped.include?(RELEASE_STATUS_MARKER_TOKEN) && marker.nil?
      abort "#{path}: malformed release-status marker at line #{index + 1}"
    end

    unless marker
      if active_id
        block << line
      else
        output << line
      end
      next
    end

    projection_id = marker[1]
    boundary = marker[2]
    unless allowed_ids.include?(projection_id)
      abort "#{path}: release-status projection #{projection_id.inspect} is not allowlisted"
    end

    if boundary == "start"
      abort "#{path}: nested release-status projection at line #{index + 1}" if active_id
      abort "#{path}: duplicate release-status projection #{projection_id}" if seen_ids.key?(projection_id)

      active_id = projection_id
      block = []
      output << line
      next
    end

    unless active_id == projection_id
      abort "#{path}: unmatched release-status end marker for #{projection_id} at line #{index + 1}"
    end
    projection = block.join
    abort "#{path}: release-status projection #{projection_id} is empty" if projection.strip.empty?
    if projection.match?(ATOMIC_GATE_ID_PATTERN)
      abort "#{path}: release-status projection #{projection_id} may not contain Gate IDs"
    end
    if projection.lines.any? { |projection_line| projection_line.match?(/^\s*\#{1,6}\s/) }
      abort "#{path}: release-status projection #{projection_id} may not contain headings"
    end

    output << "<!-- stellaris-release-status:#{projection_id}:content -->\n"
    output << line
    seen_ids[projection_id] = true
    active_id = nil
    block = []
  end

  abort "#{path}: missing release-status end marker for #{active_id}" if active_id
  return nil unless seen_ids.keys.to_set == allowed_ids.to_set
  output.join
end

def release_status_projection_only?(repo_root, base_sha, changed)
  return false if changed.empty?

  changed_marker = false
  allowed_changes = changed.all? do |path|
    if ROOT_RELEASE_STATUS_PROJECTION_PATHS.include?(path)
      base_contents = git_file_if_present(base_sha, path)
      current_file = repo_root.join(path)
      next base_contents && current_file.file? && base_contents != current_file.read
    end

    next false unless RELEASE_STATUS_PROJECTIONS.key?(path)
    base_contents = git_file_if_present(base_sha, path)
    current_file = repo_root.join(path)
    next false unless base_contents && current_file.file?

    current_contents = current_file.read
    base_projection = normalize_release_status_projections(base_contents, path)
    current_projection = normalize_release_status_projections(current_contents, path)
    marker_only = base_contents != current_contents && base_projection &&
      current_projection && base_projection == current_projection
    changed_marker ||= marker_only
    marker_only
  end
  allowed_changes && changed_marker
end

def validate_evidence_record_changes!(base_sha, changed, purpose)
  changed.each do |path|
    next unless path.start_with?(VERIFICATION_RECORD_DIRECTORY)
    next if path == "docs/verification/README.md"
    if git_file_if_present(base_sha, path)
      abort "verification records are immutable and may not be modified or deleted: #{path}"
    end
    unless purpose == "Evidence-only"
      abort "new verification records require an Evidence-only PR: #{path}"
    end
  end
end

def metadata_value(contents, label)
  DocsMetadata.read(contents, [label]).fetch(label)
rescue DocsMetadata::Error => error
  abort error.message
end

def required_metadata(contents, label, path)
  metadata_value(contents, label) || abort("#{path} is missing #{label} metadata near the top")
end

def required_adr_paths(contents, path)
  value = required_metadata(contents, "关联 ADR", path)
  if value.match?(/\AN\/?A[：:]\s*.{10,}\z/i)
    return []
  end

  paths = document_paths(value)
  unless paths.any? && paths.all? { |candidate| candidate.match?(%r{\Adocs/adr/\d{4}-[A-Za-z0-9._-]+\.md\z}) }
    abort "#{path} must use 关联 ADR: N/A: <specific reason> or numbered repository-local ADR paths"
  end
  unless value == paths.join(", ")
    abort "#{path} 关联 ADR must be a comma-separated canonical list of ADR paths"
  end
  paths
end

def normalize_snapshot(contents, mutable_labels)
  lines = contents.lines
  header = lines.first(20).join

  mutable_labels.each do |label|
    flexible_label = label.each_char.map { |character| Regexp.escape(character) }.join("\\s*")
    pattern = /(#{flexible_label}[ \t]*[：:][ \t]*(?:\r?\n[ \t]*>[ \t]*)?)([^；;。\r\n*]+)/
    occurrences = header.scan(pattern).length
    abort "duplicate #{label} metadata in snapshot" if occurrences > 1
    header = header.sub(pattern) { "#{Regexp.last_match(1)}<#{label}>" }
  end

  header + lines.drop(20).join
end

def proposed_design?(contents)
  metadata_value(contents, "文档适用性") == "Proposed"
end

def approved_design?(contents)
  metadata_value(contents, "设计评审状态") == "Approved"
end

def accepted_adr?(contents)
  metadata_value(contents, "ADR 决策状态") == "Accepted"
end

def design_adr_status?(contents, status)
  metadata_value(contents, "ADR 决策状态") == status
end

def approval_sha(value, change_class)
  abort "#{change_class} requires explicit Approved design status" unless value.match?(/\bApproved\b/)
  sha = value[/\b[0-9a-f]{40}\b/i]
  abort "#{change_class} requires the full 40-character approval commit SHA" unless sha
  abort "approval SHA is not a commit: #{sha}" unless git_success?("cat-file", "-e", "#{sha}^{commit}")
  unless git_success?("merge-base", "--is-ancestor", sha, "HEAD")
    abort "approval commit #{sha} is not an ancestor of HEAD"
  end
  sha
end

def gate_ids(value, change_class)
  ids = value.scan(ATOMIC_GATE_ID_PATTERN).uniq
  abort "#{change_class} requires at least one atomic Gate ID ending in -NN" if ids.empty?
  ids
end

def all_design_gate_ids(contents)
  contents.scan(/`(#{ATOMIC_GATE_ID_PATTERN.source})`/).flatten.uniq
end

def parse_evidence_record(contents, path)
  if contents.match?(/<!--|-->|```|~~~|<\/?[A-Za-z][^>]*>/)
    abort "#{path} may not hide evidence in HTML, comments, or fenced code blocks"
  end
  target = required_metadata(contents, "证据目标", path)
  unless target.match?(/\A[0-9a-f]{40}\z/)
    abort "#{path} must record 证据目标 as one full lowercase 40-character commit SHA"
  end
  filename_sha = File.basename(path, ".md").split("-").last
  unless filename_sha && target.start_with?(filename_sha)
    abort "#{path} filename suffix must match the leading characters of 证据目标"
  end

  overall = required_metadata(contents, "总体结果", path)
  unless %w[Passed Failed Partial].include?(overall)
    abort "#{path} has invalid 总体结果 #{overall.inspect}; expected Passed, Failed, or Partial"
  end
  waiver = required_metadata(contents, "豁免", path)
  unless waiver == "None" || waiver.length >= 10
    abort "#{path} must record 豁免: None or a specific waiver of at least 10 characters"
  end

  lines = contents.lines
  header_indexes = lines.each_index.select do |index|
    lines[index].strip.split("|").map(&:strip).reject(&:empty?) == EVIDENCE_TABLE_HEADER
  end
  abort "#{path} requires exactly one canonical Gate evidence table" unless header_indexes.length == 1

  index = header_indexes.first
  separator = lines[index + 1].to_s.strip
  separator_cells = separator.split("|").map(&:strip).reject(&:empty?)
  unless separator_cells.length == EVIDENCE_TABLE_HEADER.length &&
      separator_cells.all? { |cell| cell.match?(/\A:?-{3,}:?\z/) }
    abort "#{path} has an invalid Gate evidence table separator"
  end

  rows = {}
  lines.drop(index + 2).each do |line|
    break unless line.lstrip.start_with?("|")
    cells = line.strip.sub(/\A\|/, "").sub(/\|\z/, "").split("|", -1).map(&:strip)
    abort "#{path} Gate evidence rows must contain exactly six cells" unless cells.length == 6

    gate_match = cells[0].match(/\A`(#{ATOMIC_GATE_ID_PATTERN.source})`\z/)
    abort "#{path} has an invalid atomic Gate ID cell #{cells[0].inspect}" unless gate_match
    gate_id = gate_match[1]
    abort "#{path} contains duplicate Gate ID #{gate_id}" if rows.key?(gate_id)
    abort "#{path} Gate #{gate_id} has an empty command, expectation, observation, or artifact" if cells.values_at(1, 2, 3, 5).any?(&:empty?)

    result = cells[4]
    unless %w[Passed Failed Partial].include?(result)
      abort "#{path} Gate #{gate_id} has invalid result #{result.inspect}"
    end
    unless cells[5].match?(/\bsha256:[0-9a-f]{64}\b/)
      abort "#{path} Gate #{gate_id} Artifact / hash must contain sha256:<64 lowercase hex>"
    end
    rows[gate_id] = result
  end
  abort "#{path} Gate evidence table is empty" if rows.empty?

  expected_overall = if rows.value?("Failed")
                       "Failed"
                     elsif rows.value?("Partial")
                       "Partial"
                     else
                       "Passed"
                     end
  unless overall == expected_overall
    abort "#{path} 总体结果 #{overall} does not match row results #{expected_overall}"
  end

  { target: target, overall: overall, waiver: waiver, gates: rows }
end

def existing_evidence_records(base_sha, target)
  current_output, current_error, current_status = Open3.capture3(
    "git", "ls-tree", "-r", "--name-only", "-z", base_sha, "--", VERIFICATION_RECORD_DIRECTORY
  )
  abort "cannot list verification records at PR base: #{current_error.strip}" unless current_status.success?

  historical_output, historical_error, historical_status = Open3.capture3(
    "git", "log", "--first-parent", "--format=", "--name-only", "--diff-filter=A",
    base_sha, "--", VERIFICATION_RECORD_DIRECTORY
  )
  unless historical_status.success?
    abort "cannot inspect historical verification records: #{historical_error.strip}"
  end

  current_paths = current_output.split("\0")
  historical_paths = historical_output.lines.map(&:strip)
  paths = (current_paths + historical_paths).reject(&:empty?).uniq.select do |path|
    path.match?(VERIFICATION_RECORD_PATTERN)
  end

  paths.each_with_object([]) do |path, records|
    commits, log_error, log_status = Open3.capture3(
      "git", "log", "--first-parent", "--reverse", "--diff-filter=A", "--format=%H",
      base_sha, "--", path
    )
    abort "cannot resolve first-parent addition for #{path}: #{log_error.strip}" unless log_status.success?
    additions = commits.lines.map(&:strip).reject(&:empty?)
    unless additions.length == 1
      abort "immutable evidence record must have exactly one first-parent addition: #{path}"
    end
    addition_commit = additions.first
    addition_contents = git_file(addition_commit, path, description: "evidence addition")
    base_contents = git_file_if_present(base_sha, path)
    abort "immutable evidence record was deleted after first-parent addition: #{path}" unless base_contents
    unless addition_contents == base_contents
      abort "immutable evidence record changed after first-parent addition: #{path}"
    end

    record = parse_evidence_record(base_contents, path)
    next unless record.fetch(:target) == target
    records << [addition_commit, path, record]
  end
end

def latest_gate_results(base_sha, target, new_records)
  first_parent_output, error, status = Open3.capture3(
    "git", "rev-list", "--first-parent", "--reverse", base_sha
  )
  abort "cannot inspect first-parent history at #{base_sha}: #{error.strip}" unless status.success?
  order = first_parent_output.lines.map(&:strip).each_with_index.to_h

  latest = {}
  ordered_records = existing_evidence_records(base_sha, target).sort_by do |addition_commit, path, _record|
    [order.fetch(addition_commit) { abort "evidence addition is absent from first-parent history: #{path}" }, path]
  end
  ordered_records.group_by(&:first).each_value do |records|
    gates_in_commit = {}
    records.each do |_addition_commit, path, record|
      record.fetch(:gates).each_key do |gate_id|
        if gates_in_commit.key?(gate_id)
          abort "ambiguous latest evidence for #{gate_id}: #{gates_in_commit.fetch(gate_id)} and #{path} were added in the same first-parent commit"
        end
        gates_in_commit[gate_id] = path
      end
    end
    records.each do |_addition_commit, _path, record|
      record.fetch(:gates).each do |gate_id, result|
        latest[gate_id] = { result: result, waiver: record.fetch(:waiver) }
      end
    end
  end

  new_records.each do |_path, record|
    record.fetch(:gates).each do |gate_id, result|
      latest[gate_id] = { result: result, waiver: record.fetch(:waiver) }
    end
  end
  latest
end

def validate_declared_adrs!(contents, design_path, declared_paths, change_class)
  design_paths = required_adr_paths(contents, design_path)
  unless design_paths.to_set == declared_paths.to_set
    abort "#{change_class} Required ADR(s) must exactly match 关联 ADR in #{design_path}"
  end
  design_paths
end

def lifecycle_values(contents, path)
  LOW_CLASS_LIFECYCLE_METADATA.to_h do |label|
    [label, metadata_value(contents, label)]
  rescue DocsMetadata::Error
    abort "cannot read #{label} lifecycle metadata from #{path}"
  end
end

def validate_low_class_changes!(repo_root, base_sha, changed, change_class)
  if change_class == "D0"
    forbidden = changed.reject { |path| path.end_with?(".md") }
    unless forbidden.empty?
      abort "D0 is documentation-only and contains forbidden changes: #{forbidden.to_a.sort.join(', ')}"
    end
  end

  changed.grep(%r{\Adocs/.*\.md\z}).each do |path|
    base_contents = git_file_if_present(base_sha, path)
    current_file = repo_root.join(path)
    current_contents = current_file.file? ? current_file.read : nil

    if path.start_with?("docs/adr/") ||
        [base_contents, current_contents].compact.any? { |contents| metadata_value(contents, "文档适用性") == "Proposed" }
      abort "#{change_class} may not change Proposed design or ADR records: #{path}"
    end
    next unless base_contents && current_contents

    base_lifecycle = lifecycle_values(base_contents, "#{path} at PR base")
    current_lifecycle = lifecycle_values(current_contents, path)
    changed_labels = LOW_CLASS_LIFECYCLE_METADATA.select do |label|
      base_lifecycle.fetch(label) != current_lifecycle.fetch(label)
    end
    unless changed_labels.empty?
      abort "#{change_class} may not change design/ADR lifecycle metadata in #{path}: #{changed_labels.join(', ')}"
    end

    if path.start_with?("docs/adr/") &&
        %w[Accepted Rejected Superseded].include?(base_lifecycle.fetch("ADR 决策状态")) &&
        normalize_snapshot(base_contents, ADR_MUTABLE_METADATA) !=
          normalize_snapshot(current_contents, ADR_MUTABLE_METADATA)
      abort "#{change_class} may not rewrite a finalized ADR body: #{path}"
    end

    if base_lifecycle.fetch("设计评审状态") == "Approved" &&
        normalize_snapshot(base_contents, DESIGN_MUTABLE_METADATA) !=
          normalize_snapshot(current_contents, DESIGN_MUTABLE_METADATA)
      abort "#{change_class} may not rewrite an Approved design body: #{path}"
    end
  end
end

def reject_implementation_status_changes!(repo_root, base_sha, changed)
  evidence_changes = changed.select do |path|
    path.start_with?(VERIFICATION_RECORD_DIRECTORY) && path != "docs/verification/README.md"
  end
  unless evidence_changes.empty?
    abort "Implementation may not add or change verification records; use Evidence-only: #{evidence_changes.to_a.sort.join(', ')}"
  end

  changed.grep(%r{\Adocs/.*\.md\z}).each do |path|
    base_contents = git_file_if_present(base_sha, path)
    current_file = repo_root.join(path)
    current_contents = current_file.file? ? current_file.read : nil
    next unless current_contents

    if base_contents
      %w[验证状态 发布状态].each do |label|
        base_value = metadata_value(base_contents, label)
        current_value = metadata_value(current_contents, label)
        next if base_value == current_value
        abort "Implementation may not change #{label} from #{base_value} to #{current_value}; use Evidence-only: #{path}"
      end
    else
      verification = metadata_value(current_contents, "验证状态")
      release = metadata_value(current_contents, "发布状态")
      unless %w[Unverified N/A].include?(verification) && %w[Unreleased N/A].include?(release)
        abort "new Implementation documents may not claim verification or release status; use Evidence-only: #{path}"
      end
    end
  end
end

def validate_evidence_metadata_only!(base_contents, current_contents, path)
  %w[验证状态 发布状态].each do |label|
    required_metadata(base_contents, label, "#{path} at PR base")
    required_metadata(current_contents, label, path)
  end
  return if normalize_snapshot(base_contents, EVIDENCE_ONLY_MUTABLE_METADATA) ==
    normalize_snapshot(current_contents, EVIDENCE_ONLY_MUTABLE_METADATA)

  abort "Evidence-only may change only verification/release lifecycle metadata: #{path}"
end

def validate_evidence_target_gap!(target, base_sha)
  diff_paths(target, base_sha).each do |path|
    next if path.start_with?(VERIFICATION_RECORD_DIRECTORY)
    unless path.match?(%r{\Adocs/.*\.md\z})
      abort "evidence target #{target} is stale because runtime/configuration changed before PR base: #{path}"
    end

    target_contents = git_file_if_present(target, path)
    base_contents = git_file_if_present(base_sha, path)
    unless target_contents && base_contents &&
        normalize_snapshot(target_contents, EVIDENCE_ONLY_MUTABLE_METADATA) ==
          normalize_snapshot(base_contents, EVIDENCE_ONLY_MUTABLE_METADATA)
      abort "evidence target #{target} is stale because documentation contract changed before PR base: #{path}"
    end
  end
end

def normalize_projection_evidence_snapshot(contents, path)
  normalized = normalize_snapshot(contents, EVIDENCE_ONLY_MUTABLE_METADATA)
  return normalized unless normalized.include?(RELEASE_STATUS_MARKER_TOKEN)

  normalize_release_status_projections(normalized, path) ||
    abort("#{path}: incomplete release-status projection set")
end

def validate_projection_evidence_target_gap!(target, base_sha)
  diff_paths(target, base_sha).each do |path|
    if path.start_with?(VERIFICATION_RECORD_DIRECTORY) &&
        path != "docs/verification/README.md"
      target_record = git_file_if_present(target, path)
      base_record = git_file_if_present(base_sha, path)
      next if target_record.nil? && base_record

      abort "status projection evidence target #{target} is stale because an existing verification record changed: #{path}"
    end

    unless path.end_with?(".md")
      abort "status projection evidence target #{target} is stale because runtime/configuration changed before PR base: #{path}"
    end
    next unless path.match?(%r{\Adocs/.*\.md\z})

    target_contents = git_file_if_present(target, path)
    base_contents = git_file_if_present(base_sha, path)
    unless target_contents && base_contents &&
        normalize_projection_evidence_snapshot(target_contents, path) ==
          normalize_projection_evidence_snapshot(base_contents, path)
      abort "status projection evidence target #{target} is stale because documentation contract changed before PR base: #{path}"
    end
  end
end

def validate_status_projection_evidence!(
  base_sha,
  records,
  declared_gate_ids,
  frozen_gate_ids
)
  targets = records.map { |_path, record| record.fetch(:target) }.uniq
  abort "D4 status projection evidence records must use one evidence target" unless targets.length == 1
  target = targets.first
  unless git_success?("cat-file", "-e", "#{target}^{commit}") &&
      git_success?("merge-base", "--is-ancestor", target, base_sha)
    abort "D4 status projection evidence target must be an ancestor of PR base: #{target}"
  end
  validate_projection_evidence_target_gap!(target, base_sha)

  records.each do |path, record|
    unless record.fetch(:overall) == "Passed"
      abort "D4 status projection requires Overall Passed evidence: #{path}"
    end
    unless record.fetch(:waiver) == "None"
      abort "D4 status projection requires unwaived evidence: #{path}"
    end
  end

  cited_gate_ids = records.flat_map { |_path, record| record.fetch(:gates).keys }
  if cited_gate_ids.length != cited_gate_ids.uniq.length
    abort "D4 status projection may not cite the same Gate ID more than once"
  end
  unless cited_gate_ids.to_set == declared_gate_ids.to_set
    abort "D4 status projection Atomic Gate ID(s) must exactly match cited evidence rows"
  end
  unless cited_gate_ids.to_set == frozen_gate_ids.to_set
    missing = frozen_gate_ids - cited_gate_ids
    extra = cited_gate_ids - frozen_gate_ids
    details = []
    details << "missing: #{missing.sort.join(', ')}" unless missing.empty?
    details << "unexpected: #{extra.sort.join(', ')}" unless extra.empty?
    abort "D4 status projection evidence must cover every frozen Gate (#{details.join('; ')})"
  end

  latest = latest_gate_results(base_sha, target, [])
  nonqualifying = cited_gate_ids.reject do |gate_id|
    result = latest[gate_id]
    result && result.fetch(:result) == "Passed" && result.fetch(:waiver) == "None"
  end
  unless nonqualifying.empty?
    abort "D4 status projection requires latest Passed, unwaived evidence for: #{nonqualifying.sort.join(', ')}"
  end
end

def validate_status_transition!(base_contents, current_contents, path, passed_gate_ids, all_gate_ids)
  base_verification = required_metadata(base_contents, "验证状态", "#{path} at PR base")
  current_verification = required_metadata(current_contents, "验证状态", path)
  base_release = required_metadata(base_contents, "发布状态", "#{path} at PR base")
  current_release = required_metadata(current_contents, "发布状态", path)

  unless %w[Unverified Partially\ verified Verified N/A].include?(current_verification)
    abort "Evidence-only has invalid 验证状态 transition: #{path} (#{base_verification} -> #{current_verification})"
  end
  if (base_verification == "N/A") != (current_verification == "N/A")
    abort "Evidence-only may not change whether 验证状态 is N/A: #{path}"
  end
  unless %w[Unreleased Prerelease Stable N/A].include?(current_release)
    abort "Evidence-only has invalid 发布状态 transition: #{path} (#{base_release} -> #{current_release})"
  end
  if (base_release == "N/A") != (current_release == "N/A")
    abort "Evidence-only may not change whether 发布状态 is N/A: #{path}"
  end

  if current_verification == "Partially verified" && passed_gate_ids.empty?
    abort "Partially verified requires at least one Passed Gate for evidence target: #{path}"
  end
  if current_verification == "Verified"
    missing = all_gate_ids - passed_gate_ids
    unless missing.empty?
      abort "Verified requires Passed evidence for every frozen Gate; missing: #{missing.sort.join(', ')}"
    end
  end
  if current_release == "Stable" && current_verification != "Verified"
    abort "Stable requires 验证状态: Verified in the same document: #{path}"
  end
end

def validate_gates!(ids, current_design, design_path, frozen_design: nil, approval_sha: nil)
  ids.each do |gate_id|
    marker = "`#{gate_id}`"
    abort "atomic Gate ID #{gate_id} is absent from current design #{design_path}" unless current_design.include?(marker)
    next unless frozen_design

    unless frozen_design.include?(marker)
      abort "atomic Gate ID #{gate_id} was absent from #{design_path} at approval commit #{approval_sha}"
    end
  end
end

def validate_approved_design!(repo_root, design_path, approval_field, change_class)
  design_file = repository_document(repo_root, design_path)
  current_design = design_file.read
  sha = approval_sha(approval_field, change_class)
  frozen_design = git_file(sha, design_path, description: "approval commit")

  abort "current design is not Proposed: #{design_path}" unless proposed_design?(current_design)
  abort "current design is not Approved: #{design_path}" unless approved_design?(current_design)
  abort "approval commit does not contain the frozen Proposed design: #{design_path}" unless proposed_design?(frozen_design)
  unless metadata_value(frozen_design, "设计评审状态") == "Draft"
    abort "approval commit must contain the frozen Draft design: #{design_path}"
  end
  unless required_metadata(frozen_design, "设计批准", "#{design_path} at approval commit") == "N/A"
    abort "frozen Draft must record 设计批准: N/A at approval commit: #{design_path}"
  end

  approval_record = required_metadata(current_design, "设计批准", design_path)
  unless approval_record.match?(/\b#{Regexp.escape(sha)}\b/i)
    abort "current design does not record approval commit #{sha} in 设计批准 metadata"
  end

  unless normalize_snapshot(current_design, DESIGN_MUTABLE_METADATA) ==
      normalize_snapshot(frozen_design, DESIGN_MUTABLE_METADATA)
    abort "Approved design body or immutable metadata changed after approval: #{design_path}"
  end

  [sha, current_design, frozen_design]
end

def validate_design_transition!(base_design, current_design, path)
  current_applicability = required_metadata(current_design, "文档适用性", path)
  current_review = required_metadata(current_design, "设计评审状态", path)
  unless %w[Proposed Historical].include?(current_applicability)
    abort "Design-only may not use a Current document as its design: #{path}"
  end

  if %w[Draft Approved].include?(current_review) && current_applicability != "Proposed"
    abort "#{current_review} design must remain Proposed: #{path}"
  end
  if %w[Rejected Superseded].include?(current_review) && current_applicability != "Historical"
    abort "#{current_review} design must be Historical: #{path}"
  end

  base_review = nil
  if base_design
    base_applicability = required_metadata(base_design, "文档适用性", "#{path} at PR base")
    abort "Design-only may not downgrade a Current base document into a proposal: #{path}" if base_applicability == "Current"
    base_review = required_metadata(base_design, "设计评审状态", "#{path} at PR base")
  end

  current_approval = required_metadata(current_design, "设计批准", path)
  if %w[Draft Rejected].include?(current_review) && current_approval != "N/A"
    abort "#{current_review} design must record 设计批准: N/A: #{path}"
  end
  if current_review == "Superseded" && base_review == "Draft" && current_approval != "N/A"
    abort "unapproved Superseded design must record 设计批准: N/A: #{path}"
  end

  DESIGN_ONLY_INITIAL_LIFECYCLE.each do |label, initial_value|
    current_value = required_metadata(current_design, label, path)
    if base_design
      base_value = required_metadata(base_design, label, "#{path} at PR base")
      unless current_value == base_value
        abort "Design-only may not change #{label} from #{base_value} to #{current_value}: #{path}"
      end
    elsif current_value != initial_value
      abort "new Design-only record must use #{label}: #{initial_value}: #{path}"
    end
  end

  allowed = DESIGN_REVIEW_TRANSITIONS.fetch(base_review) do
    abort "unsupported base design review status #{base_review.inspect}: #{path}"
  end
  unless allowed.include?(current_review)
    abort "invalid design review transition #{base_review || 'new'} -> #{current_review}: #{path}"
  end

  finalized = %w[Approved Rejected Superseded]
  return unless base_design && (finalized.include?(base_review) || %w[Rejected Superseded].include?(current_review))
  return if normalize_snapshot(base_design, DESIGN_ONLY_MUTABLE_METADATA) ==
    normalize_snapshot(current_design, DESIGN_ONLY_MUTABLE_METADATA)

  abort "finalized design body or immutable metadata may not be rewritten: #{path}"
end

def validate_adr_transition!(base_adr, current_adr, path)
  current_status = required_metadata(current_adr, "ADR 决策状态", path)
  current_applicability = required_metadata(current_adr, "文档适用性", path)

  if current_status == "Proposed" && current_applicability != "Proposed"
    abort "Proposed ADR must have Proposed applicability: #{path}"
  end
  if %w[Rejected Superseded].include?(current_status) && current_applicability != "Historical"
    abort "#{current_status} ADR must be Historical: #{path}"
  end

  base_status = base_adr && required_metadata(base_adr, "ADR 决策状态", "#{path} at PR base")

  DESIGN_ONLY_INITIAL_LIFECYCLE.each do |label, initial_value|
    current_value = required_metadata(current_adr, label, path)
    if base_adr
      base_value = required_metadata(base_adr, label, "#{path} at PR base")
      unless current_value == base_value
        abort "Design-only may not change #{label} from #{base_value} to #{current_value}: #{path}"
      end
    elsif current_value != initial_value
      abort "new Design-only record must use #{label}: #{initial_value}: #{path}"
    end
  end

  allowed = ADR_DECISION_TRANSITIONS.fetch(base_status) do
    abort "unsupported base ADR decision status #{base_status.inspect}: #{path}"
  end
  unless allowed.include?(current_status)
    abort "invalid ADR decision transition #{base_status || 'new'} -> #{current_status}: #{path}"
  end

  finalized = %w[Accepted Rejected Superseded]
  return unless base_adr && (finalized.include?(base_status) || %w[Rejected Superseded].include?(current_status))

  return if normalize_snapshot(base_adr, ADR_DESIGN_ONLY_MUTABLE_METADATA) ==
    normalize_snapshot(current_adr, ADR_DESIGN_ONLY_MUTABLE_METADATA)

  abort "finalized ADR body or immutable metadata may not be rewritten: #{path}"
end

abort "Pull Request body may not contain HTML comments" if body.match?(/<!--|-->/)

required_headings = ["Summary", "Design contract", "Impact", "Validation", "Checklist"]
required_headings.each do |heading|
  count = body.scan(/^## #{Regexp.escape(heading)}[ \t]*\r?$/).length
  abort "PR body requires exactly one '#{heading}' section; found #{count}" unless count == 1
end

design_contract = body.match(
  /^## Design contract[ \t]*\r?$\n(.*?)(?=^##[ \t]+|\z)/m
)&.[](1)
abort "cannot parse Design contract section" unless design_contract

field_labels = [
  "Change class",
  "PR purpose",
  "Proposed design",
  "Required ADR(s)",
  "Design review status and approval commit",
  "Atomic Gate ID(s)",
  "Evidence record(s)",
  "Current authoritative documents affected",
  "Applicability, design-review, ADR, delivery, verification, and release status changes"
]
fields = field_labels.to_h do |label|
  matches = design_contract.scan(/^- #{Regexp.escape(label)}:[ \t]*(\S.*?)[ \t]*\r?$/)
  abort "Design contract requires exactly one non-empty field '#{label}'; found #{matches.length}" unless matches.length == 1
  [label, matches.first.first.strip]
end

change_class = fields.fetch("Change class")
abort "Change class must be exactly D0, D1, D2, D3, or D4" unless change_class.match?(/\AD[0-4]\z/)

purpose = fields.fetch("PR purpose")
unless %w[Design-only Implementation Evidence-only].include?(purpose)
  abort "PR purpose must be exactly Design-only, Implementation, or Evidence-only"
end

global_base_sha = pr_base_sha!
global_changed_paths = changed_paths(global_base_sha)
validate_evidence_record_changes!(global_base_sha, global_changed_paths, purpose)

if %w[D0 D1].include?(change_class)
  abort "#{purpose} is only valid for D2, D3, or D4" unless purpose == "Implementation"
  [
    "Proposed design",
    "Required ADR(s)",
    "Design review status and approval commit",
    "Atomic Gate ID(s)",
    "Evidence record(s)"
  ].each do |label|
    unless fields.fetch(label).match?(/\ANot applicable:\s*\S/i)
      abort "#{change_class} requires '#{label}: Not applicable: <reason>'"
    end
  end
  validate_low_class_changes!(repo_root, global_base_sha, global_changed_paths, change_class)
  puts "validated #{change_class} #{purpose} documentation contract"
  exit
end

if purpose == "Evidence-only" && change_class != "D4"
  abort "Evidence-only must use Change class D4"
end

design_value = fields.fetch("Proposed design")
design_paths = document_paths(design_value)
abort "#{change_class} permits at most one repository-local Proposed design path" if design_paths.length > 1
design_path = design_paths.first
if design_path
  abort "an ADR is not the Proposed design document" if design_path.start_with?("docs/adr/")
elsif !design_value.match?(/\ANot applicable:\s*\S/i)
  abort "missing Proposed design path; use 'Not applicable: <reason>' only for an ADR-only Design-only PR"
end

adr_value = fields.fetch("Required ADR(s)")
adr_paths = []
if change_class == "D2"
  unless adr_value.match?(/\ANot required:\s*.{10,}\z/i)
    abort "D2 requires 'Required ADR(s): Not required: <specific reason>'; reclassify an ADR-triggering change as D3"
  end
else
  all_adr_field_paths = document_paths(adr_value)
  adr_paths = all_adr_field_paths.select do |path|
    path.match?(%r{\Adocs/adr/\d{4}-[A-Za-z0-9._-]+\.md\z})
  end
  if adr_paths.empty? || adr_paths.length != all_adr_field_paths.length
    abort "#{change_class} requires at least one repository-local numbered ADR path"
  end
  unless adr_value == adr_paths.join(", ")
    abort "#{change_class} Required ADR(s) must be a canonical comma-separated ADR path list"
  end
end


evidence_value = fields.fetch("Evidence record(s)")
evidence_paths = verification_record_paths(evidence_value)
status_projection_only = false
status_projection_evidence = []
if purpose == "Evidence-only"
  all_evidence_field_paths = document_paths(evidence_value)
  if evidence_paths.empty? || evidence_paths.length != all_evidence_field_paths.length
    abort "Evidence-only requires at least one repository-local docs/verification record path"
  end
  unless evidence_value == evidence_paths.join(", ")
    abort "Evidence record(s) must be a canonical comma-separated record path list"
  end
elsif purpose == "Implementation" && change_class == "D4"
  status_projection_only = release_status_projection_only?(
    repo_root, global_base_sha, global_changed_paths
  )
  if evidence_value.match?(/\ANot applicable:\s*\S/i)
    if status_projection_only
      abort "D4 status-projection Implementation requires at least one merged Evidence record"
    end
  else
    all_evidence_field_paths = document_paths(evidence_value)
    if evidence_paths.empty? || evidence_paths.length != all_evidence_field_paths.length
      abort "D4 Implementation Evidence record(s) must contain repository-local verification records only"
    end
    unless evidence_value == evidence_paths.join(", ")
      abort "D4 Implementation Evidence record(s) must be a canonical comma-separated record path list"
    end

    merged_evidence_records = evidence_paths.map do |path|
      abort "invalid verification record filename: #{path}" unless path.match?(VERIFICATION_RECORD_PATTERN)
      base_record = git_file_if_present(global_base_sha, path)
      abort "D4 Implementation evidence must already exist at PR base: #{path}" unless base_record
      current_record = repository_document(repo_root, path).read
      unless current_record == base_record
        abort "D4 Implementation evidence must remain immutable: #{path}"
      end
      [path, parse_evidence_record(base_record, path)]
    end
    status_projection_evidence = merged_evidence_records if status_projection_only
  end
elsif !evidence_value.match?(/\ANot applicable:\s*\S/i)
  abort "#{purpose} requires 'Evidence record(s): Not applicable: <reason>'"
end

[
  "Current authoritative documents affected",
  "Applicability, design-review, ADR, delivery, verification, and release status changes"
].each do |label|
  abort "#{change_class} may not use N/A for #{label}" if fields.fetch(label).match?(/\AN\/?A\b/i)
end

if purpose == "Design-only"
  abort "D2 Design-only requires a Proposed design" if change_class == "D2" && design_path.nil?
  abort "Design-only must declare a design or ADR" if design_path.nil? && adr_paths.empty?

  base_sha = pr_base_sha!

  changed = changed_paths(base_sha)
  record_paths = [design_path, *adr_paths].compact.to_set
  allowed_paths = DESIGN_ONLY_INDEX_PATHS | record_paths
  unexpected_paths = changed - allowed_paths
  unless unexpected_paths.empty?
    abort "Design-only contains forbidden changes: #{unexpected_paths.to_a.sort.join(', ')}"
  end
  if (changed & record_paths).empty?
    abort "Design-only must change at least one declared design or ADR record"
  end

  current_design = nil
  if design_path
    design_file = repository_document(repo_root, design_path)
    current_design = design_file.read
    base_design = git_file_if_present(base_sha, design_path)
    validate_design_transition!(base_design, current_design, design_path)

    review_status = required_metadata(current_design, "设计评审状态", design_path)
    review_field = fields.fetch("Design review status and approval commit")
    unless review_field.match?(/\b#{Regexp.escape(review_status)}\b/)
      abort "PR design review field does not match #{review_status} metadata in #{design_path}"
    end

    ids = gate_ids(fields.fetch("Atomic Gate ID(s)"), change_class)
    validate_gates!(ids, current_design, design_path)

    if review_status == "Approved"
      sha, approved_design, frozen_design = validate_approved_design!(
        repo_root, design_path, review_field, change_class
      )
      unless git_success?("merge-base", "--is-ancestor", sha, base_sha)
        abort "Design-only approval commit #{sha} must be an ancestor of PR base #{base_sha}"
      end
      unless base_design && normalize_snapshot(base_design, DESIGN_MUTABLE_METADATA) ==
          normalize_snapshot(frozen_design, DESIGN_MUTABLE_METADATA)
        abort "Draft design at PR base does not match approval snapshot: #{design_path}"
      end
      validate_gates!(
        ids, approved_design, design_path, frozen_design: frozen_design, approval_sha: sha
      )
    end
  else
    unless fields.fetch("Design review status and approval commit").match?(/\ANot applicable:\s*\S/i)
      abort "ADR-only Design-only requires 'Design review status and approval commit: Not applicable: <reason>'"
    end
    unless fields.fetch("Atomic Gate ID(s)").match?(/\ANot applicable:\s*\S/i)
      abort "ADR-only Design-only requires 'Atomic Gate ID(s): Not applicable: <reason>'"
    end
  end

  adr_statuses = adr_paths.map do |adr_path|
    adr_file = repository_document(repo_root, adr_path)
    current_adr = adr_file.read
    base_adr = git_file_if_present(base_sha, adr_path)
    validate_adr_transition!(base_adr, current_adr, adr_path)
    required_metadata(current_adr, "ADR 决策状态", adr_path)
  end

  if change_class == "D2"
    validate_declared_adrs!(current_design, design_path, [], change_class)
    unless design_adr_status?(current_design, "N/A")
      abort "D2 design must record ADR 决策状态: N/A; reclassify an ADR-backed change as D3"
    end
  elsif current_design
    validate_declared_adrs!(current_design, design_path, adr_paths, change_class)
    abort "Design-only requires all declared ADRs to share one decision status" unless adr_statuses.uniq.length == 1
    expected_status = adr_statuses.first
    unless design_adr_status?(current_design, expected_status)
      abort "design ADR 决策状态 must match declared ADR status #{expected_status}"
    end
    if required_metadata(current_design, "设计评审状态", design_path) == "Approved"
      unless expected_status == "Accepted"
        abort "Approved #{change_class} design requires every associated ADR to be Accepted"
      end
      approval_commit = approval_sha(fields.fetch("Design review status and approval commit"), change_class)
      frozen_design = git_file(approval_commit, design_path, description: "approval commit")
      validate_declared_adrs!(frozen_design, design_path, adr_paths, change_class)
      adr_paths.each do |adr_path|
        frozen_adr = git_file(approval_commit, adr_path, description: "approval commit")
        unless accepted_adr?(frozen_adr)
          abort "ADR was not Accepted at design approval commit #{approval_commit}: #{adr_path}"
        end
      end
    end
  end

  puts "validated #{change_class} #{purpose} documentation contract"
  exit
end

if purpose == "Evidence-only"
  abort "D4 Evidence-only requires exactly one repository-local Approved design path" unless design_path

  evidence_base = pr_base_sha!
  approval_field = fields.fetch("Design review status and approval commit")
  approval_commit, current_design, frozen_design = validate_approved_design!(
    repo_root, design_path, approval_field, change_class
  )
  unless git_success?("merge-base", "--is-ancestor", approval_commit, evidence_base)
    abort "approval commit #{approval_commit} must be an ancestor of PR base #{evidence_base}"
  end

  base_design = git_file_if_present(evidence_base, design_path)
  unless base_design && approved_design?(base_design)
    abort "PR base must already contain the Approved design: #{design_path}"
  end
  [current_design, frozen_design, base_design].each do |contents|
    validate_declared_adrs!(contents, design_path, adr_paths, change_class)
    unless design_adr_status?(contents, "Accepted")
      abort "D4 Evidence-only design must record ADR 决策状态: Accepted"
    end
  end

  adr_paths.each do |adr_path|
    current_adr = repository_document(repo_root, adr_path).read
    frozen_adr = git_file(approval_commit, adr_path, description: "approval commit")
    base_adr = git_file_if_present(evidence_base, adr_path)
    unless base_adr && accepted_adr?(base_adr) && accepted_adr?(current_adr) && accepted_adr?(frozen_adr)
      abort "Evidence-only requires an Accepted ADR at approval, PR base, and HEAD: #{adr_path}"
    end
    unless normalize_snapshot(base_adr, ADR_MUTABLE_METADATA) ==
        normalize_snapshot(frozen_adr, ADR_MUTABLE_METADATA) &&
        normalize_snapshot(current_adr, ADR_MUTABLE_METADATA) ==
          normalize_snapshot(frozen_adr, ADR_MUTABLE_METADATA)
      abort "Accepted ADR body or immutable metadata changed after design approval: #{adr_path}"
    end
  end

  current_doc_paths = document_paths(fields.fetch("Current authoritative documents affected"))
  changed = changed_paths(evidence_base)
  allowed_paths = Set.new([design_path, *adr_paths, *current_doc_paths, *evidence_paths])
  unexpected_paths = changed - allowed_paths
  unless unexpected_paths.empty?
    abort "Evidence-only contains forbidden changes: #{unexpected_paths.to_a.sort.join(', ')}"
  end
  missing_records = evidence_paths.to_set - changed
  unless missing_records.empty?
    abort "Evidence-only must add every declared evidence record: #{missing_records.to_a.sort.join(', ')}"
  end

  new_records = evidence_paths.map do |path|
    abort "invalid verification record filename: #{path}" unless path.match?(VERIFICATION_RECORD_PATTERN)
    abort "Evidence-only records are immutable once merged: #{path}" if git_file_if_present(evidence_base, path)
    record = parse_evidence_record(repository_document(repo_root, path).read, path)
    unless git_success?("cat-file", "-e", "#{record.fetch(:target)}^{commit}") &&
        git_success?("merge-base", "--is-ancestor", record.fetch(:target), evidence_base)
      abort "evidence target must be a commit that is an ancestor of PR base: #{record.fetch(:target)}"
    end
    [path, record]
  end
  targets = new_records.map { |_path, record| record.fetch(:target) }.uniq
  abort "all Evidence-only records must use one evidence target" unless targets.length == 1
  evidence_target = targets.first
  validate_evidence_target_gap!(evidence_target, evidence_base)

  declared_ids = gate_ids(fields.fetch("Atomic Gate ID(s)"), change_class)
  all_new_record_ids = new_records.flat_map { |_path, record| record.fetch(:gates).keys }
  if all_new_record_ids.length != all_new_record_ids.uniq.length
    abort "one Evidence-only PR may not record the same Gate ID more than once"
  end
  new_record_ids = all_new_record_ids.uniq
  unless declared_ids.to_set == new_record_ids.to_set
    abort "Atomic Gate ID(s) must exactly match Gate rows in new evidence records"
  end

  all_gate_ids = all_design_gate_ids(frozen_design)
  abort "Approved design contains no atomic Gate IDs: #{design_path}" if all_gate_ids.empty?
  unknown_ids = new_record_ids - all_gate_ids
  unless unknown_ids.empty?
    abort "evidence contains Gate IDs absent from frozen design: #{unknown_ids.sort.join(', ')}"
  end

  latest_results = latest_gate_results(evidence_base, evidence_target, new_records)
  passed_gate_ids = latest_results.select do |_gate_id, result|
    result.fetch(:result) == "Passed" && result.fetch(:waiver) == "None"
  end.keys

  lifecycle_paths = Set.new([design_path, *adr_paths, *current_doc_paths])
  lifecycle_paths.each do |path|
    base_contents = git_file_if_present(evidence_base, path)
    current_contents = repository_document(repo_root, path).read
    abort "Evidence-only may not add lifecycle documents: #{path}" unless base_contents
    validate_evidence_metadata_only!(base_contents, current_contents, path)
    validate_status_transition!(
      base_contents, current_contents, path, passed_gate_ids, all_gate_ids
    )
  end

  puts "validated #{change_class} #{purpose} documentation contract for #{evidence_target}"
  exit
end

abort "#{change_class} Implementation requires exactly one repository-local Proposed design path" unless design_path

approval_field = fields.fetch("Design review status and approval commit")
sha, current_design, frozen_design = validate_approved_design!(
  repo_root, design_path, approval_field, change_class
)

implementation_base = pr_base_sha!
reject_implementation_status_changes!(
  repo_root, implementation_base, changed_paths(implementation_base)
)
unless git_success?("merge-base", "--is-ancestor", sha, implementation_base)
  abort "approval commit #{sha} must be an ancestor of PR base #{implementation_base}"
end
base_design = git_file_if_present(implementation_base, design_path)
unless base_design && approved_design?(base_design)
  abort "PR base must already contain the Approved design: #{design_path}"
end
base_approval_record = required_metadata(base_design, "设计批准", "#{design_path} at PR base")
unless base_approval_record.match?(/\b#{Regexp.escape(sha)}\b/i)
  abort "Approved design at PR base does not record approval commit #{sha}: #{design_path}"
end
current_approval_record = required_metadata(current_design, "设计批准", design_path)
unless current_approval_record == base_approval_record
  abort "Implementation may not rewrite the design approval record: #{design_path}"
end
unless normalize_snapshot(base_design, DESIGN_MUTABLE_METADATA) ==
    normalize_snapshot(frozen_design, DESIGN_MUTABLE_METADATA)
  abort "Approved design at PR base does not match its frozen snapshot: #{design_path}"
end

ids = gate_ids(fields.fetch("Atomic Gate ID(s)"), change_class)
validate_gates!(ids, current_design, design_path, frozen_design: frozen_design, approval_sha: sha)
if status_projection_only
  frozen_gate_ids = all_design_gate_ids(frozen_design)
  abort "Approved design contains no atomic Gate IDs: #{design_path}" if frozen_gate_ids.empty?
  validate_status_projection_evidence!(
    implementation_base,
    status_projection_evidence,
    ids,
    frozen_gate_ids
  )
end

if change_class == "D2"
  validate_declared_adrs!(current_design, design_path, [], change_class)
  validate_declared_adrs!(frozen_design, design_path, [], change_class)
  validate_declared_adrs!(base_design, design_path, [], change_class)
  unless design_adr_status?(current_design, "N/A") && design_adr_status?(frozen_design, "N/A")
    abort "D2 design must record ADR 决策状态: N/A; reclassify an ADR-backed change as D3"
  end
else
  validate_declared_adrs!(current_design, design_path, adr_paths, change_class)
  validate_declared_adrs!(frozen_design, design_path, adr_paths, change_class)
  validate_declared_adrs!(base_design, design_path, adr_paths, change_class)
  unless design_adr_status?(current_design, "Accepted") && design_adr_status?(frozen_design, "Accepted")
    abort "#{change_class} design must record ADR 决策状态: Accepted"
  end

  adr_paths.each do |adr_path|
    adr_file = repository_document(repo_root, adr_path)
    current_adr = adr_file.read
    frozen_adr = git_file(sha, adr_path, description: "approval commit")
    base_adr = git_file_if_present(implementation_base, adr_path)
    unless base_adr && accepted_adr?(base_adr)
      abort "PR base must already contain the Accepted ADR: #{adr_path}"
    end
    unless normalize_snapshot(base_adr, ADR_MUTABLE_METADATA) ==
        normalize_snapshot(frozen_adr, ADR_MUTABLE_METADATA)
      abort "Accepted ADR at PR base does not match its frozen snapshot: #{adr_path}"
    end
    abort "ADR is not currently Accepted: #{adr_path}" unless accepted_adr?(current_adr)
    unless accepted_adr?(frozen_adr)
      abort "ADR was not Accepted at design approval commit #{sha}: #{adr_path}"
    end
    unless normalize_snapshot(current_adr, ADR_MUTABLE_METADATA) ==
        normalize_snapshot(frozen_adr, ADR_MUTABLE_METADATA)
      abort "Accepted ADR body or immutable metadata changed after design approval: #{adr_path}"
    end

    basename = File.basename(adr_path)
    unless current_design.include?(basename) && frozen_design.include?(basename)
      abort "Approved design must reference #{basename}"
    end
  end
end

puts "validated #{change_class} #{purpose} documentation contract"
