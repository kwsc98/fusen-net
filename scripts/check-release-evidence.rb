#!/usr/bin/env ruby
# SPDX-License-Identifier: Apache-2.0 OR MIT

require "open3"
require_relative "docs_metadata"

PLAN_PATH = "docs/distributed-network-plan.md"
EVIDENCE_DIRECTORY = "docs/verification"
EVIDENCE_PATH_PATTERN = %r{\Adocs/verification/\d{4}-\d{2}-\d{2}-[A-Za-z0-9._-]+-[0-9a-f]{7,40}\.md\z}
GATE_ID_PATTERN = /[A-Z][A-Z0-9]*(?:-[A-Z0-9]+)+-\d{2}/
EVIDENCE_TABLE_HEADER = [
  "Gate ID",
  "命令或场景",
  "预期",
  "实际",
  "结果",
  "Artifact / hash"
].freeze
EVIDENCE_MUTABLE_METADATA = %w[验证状态 发布状态 最后核对].freeze
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
RELEASE_STATUS_MARKER_TOKEN = "stellaris-release-status:"
RELEASE_STATUS_MARKER_PATTERN = /\A<!-- stellaris-release-status:([a-z0-9]+(?:-[a-z0-9]+)*):(start|end) -->\z/

def git_output(*arguments)
  stdout, stderr, status = Open3.capture3("git", *arguments)
  abort "git #{arguments.join(' ')} failed: #{stderr.strip}" unless status.success?
  stdout
end

def metadata(contents, labels, path)
  DocsMetadata.read(contents, labels)
rescue DocsMetadata::Error => error
  abort "#{path}: #{error.message}"
end

def changed_paths(from_commit, to_commit)
  git_output(
    "diff", "--name-only", "--no-renames", "-z", "#{from_commit}..#{to_commit}"
  ).split("\0").reject(&:empty?)
end

def git_file_if_present(commit, path)
  contents, _stderr, status = Open3.capture3("git", "show", "#{commit}:#{path}")
  status.success? ? contents : nil
end

def git_tree_paths(commit, pathspec)
  git_output(
    "ls-tree", "-r", "--name-only", "-z", commit, "--", pathspec
  ).split("\0").reject(&:empty?)
end

def normalize_release_status_projections(contents, path)
  return contents unless contents.include?(RELEASE_STATUS_MARKER_TOKEN)

  allowed_ids = RELEASE_STATUS_PROJECTIONS.fetch(path, [])
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
    if projection.match?(GATE_ID_PATTERN)
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
  output.join
end

def normalize_evidence_snapshot(contents, path)
  lines = contents.lines
  header = lines.first(20).join
  EVIDENCE_MUTABLE_METADATA.each do |label|
    flexible_label = label.each_char.map { |character| Regexp.escape(character) }.join("\\s*")
    pattern = /(#{flexible_label}[ \t]*[：:][ \t]*(?:\r?\n[ \t]*>[ \t]*)?)([^；;。\r\n*]+)/
    occurrences = header.scan(pattern).length
    abort "duplicate #{label} metadata in release snapshot" if occurrences > 1
    header = header.sub(pattern) { "#{Regexp.last_match(1)}<#{label}>" }
  end
  normalized = header + lines.drop(20).join
  normalize_release_status_projections(normalized, path)
end

def evidence_target_is_current?(target, release_commit, record_path)
  changed_paths(target, release_commit).each do |path|
    if path.start_with?("docs/verification/") && path != "docs/verification/README.md"
      target_record = git_file_if_present(target, path)
      release_record = git_file_if_present(release_commit, path)
      if target_record.nil? && release_record
        next
      end
      warn "ignoring stale evidence #{record_path}: an existing verification record changed: #{path}"
      return false
    end

    unless path.end_with?(".md")
      warn "ignoring stale evidence #{record_path}: non-lifecycle path changed: #{path}"
      return false
    end

    # README, changelog, and other non-normative Markdown may project already
    # recorded status. Normative docs must differ only in lifecycle metadata.
    next unless path.match?(%r{\Adocs/.*\.md\z})

    target_contents = git_file_if_present(target, path)
    release_contents = git_file_if_present(release_commit, path)
    unless target_contents && release_contents &&
        normalize_evidence_snapshot(target_contents, path) ==
          normalize_evidence_snapshot(release_contents, path)
      warn "ignoring stale evidence #{record_path}: documentation contract changed: #{path}"
      return false
    end
  end
  true
end

def evidence_rows(contents, path, overall)
  lines = contents.lines
  header_indexes = lines.each_index.select do |index|
    columns = lines[index].strip.split("|", -1)[1...-1]&.map(&:strip)
    columns == EVIDENCE_TABLE_HEADER
  end
  abort "#{path}: requires exactly one canonical Gate evidence table" unless header_indexes.length == 1

  separator_index = header_indexes.first + 1
  separator_columns = lines[separator_index].to_s.strip.split("|", -1)[1...-1]&.map(&:strip)
  unless separator_columns&.length == EVIDENCE_TABLE_HEADER.length &&
      separator_columns.all? { |column| column.match?(/\A:?-{3,}:?\z/) }
    abort "#{path}: invalid Gate evidence table separator"
  end

  index = separator_index + 1
  rows = {}
  while index < lines.length && lines[index].lstrip.start_with?("|")
    columns = lines[index].strip.split("|", -1)[1...-1]&.map(&:strip)
    abort "#{path}: malformed Gate evidence row at line #{index + 1}" unless columns&.length == 6

    gate_match = columns[0].match(/\A`(#{GATE_ID_PATTERN.source})`\z/)
    abort "#{path}: invalid Gate ID at line #{index + 1}" unless gate_match
    gate_id = gate_match[1]
    abort "#{path}: duplicate Gate ID #{gate_id}" if rows.key?(gate_id)
    if columns.values_at(1, 2, 3, 5).any?(&:empty?)
      abort "#{path}: Gate #{gate_id} has an empty command, expectation, observation, or artifact"
    end

    result = columns[4]
    unless %w[Passed Failed Partial].include?(result)
      abort "#{path}: invalid result #{result.inspect} for #{gate_id}"
    end
    artifact = columns[5]
    unless artifact.match?(/\bsha256:[0-9a-f]{64}\b/)
      abort "#{path}: Gate #{gate_id} artifact must include sha256:<64 lowercase hex>"
    end

    rows[gate_id] = result
    index += 1
  end
  abort "#{path}: Gate evidence table is empty" if rows.empty?

  expected_overall = if rows.value?("Failed")
                       "Failed"
                     elsif rows.value?("Partial")
                       "Partial"
                     else
                       "Passed"
                     end
  unless overall == expected_overall
    abort "#{path}: 总体结果 #{overall} does not match row results #{expected_overall}"
  end
  rows
end

is_stable = ENV.fetch("IS_STABLE", "")
abort "IS_STABLE must be exactly true or false" unless %w[true false].include?(is_stable)
unless is_stable == "true"
  puts "prerelease does not claim stable Gate qualification"
  exit
end

release_commit = ENV.fetch("RELEASE_COMMIT", "")
unless release_commit.match?(/\A[0-9a-f]{40}\z/)
  abort "stable release requires RELEASE_COMMIT as a full lowercase commit SHA"
end
head = git_output("rev-parse", "HEAD").strip
abort "checked-out HEAD #{head} does not match release commit #{release_commit}" unless head == release_commit

plan = git_file_if_present(release_commit, PLAN_PATH)
abort "missing Current release plan in release commit: #{PLAN_PATH}" unless plan
plan_metadata = metadata(plan, %w[文档适用性 验证状态 发布状态], PLAN_PATH)
abort "#{PLAN_PATH}: stable release plan must be Current" unless plan_metadata.fetch("文档适用性") == "Current"
abort "#{PLAN_PATH}: stable release plan must be Verified" unless plan_metadata.fetch("验证状态") == "Verified"
abort "#{PLAN_PATH}: stable release plan must declare Stable" unless plan_metadata.fetch("发布状态") == "Stable"

incomplete_release_documents = []
release_documents = git_tree_paths(release_commit, "docs").select do |path|
  path.start_with?("docs/") && path.end_with?(".md")
end.sort
release_documents.each do |path|
  contents = git_file_if_present(release_commit, path)
  abort "cannot read release document from release commit: #{path}" unless contents
  values = metadata(contents, %w[文档适用性 交付状态 验证状态 发布状态], path)
  next unless values.fetch("文档适用性") == "Current" && values.fetch("交付状态") == "Implemented"
  next if values.fetch("验证状态") == "Verified" && values.fetch("发布状态") == "Stable"

  incomplete_release_documents << "#{path} (#{values.fetch('验证状态')} / #{values.fetch('发布状态')})"
end
unless incomplete_release_documents.empty?
  abort "stable release has incomplete Current implementation documents: #{incomplete_release_documents.join(', ')}"
end

required_gates = plan.scan(/`(#{GATE_ID_PATTERN.source})`/).flatten.uniq.sort
abort "#{PLAN_PATH}: stable release plan defines no atomic Gate IDs" if required_gates.empty?

current_records = git_tree_paths(release_commit, EVIDENCE_DIRECTORY)
  .reject { |path| File.basename(path) == "README.md" }
historical_records = git_output(
  "log", "--first-parent", "--format=", "--name-only", "--diff-filter=A",
  release_commit, "--", EVIDENCE_DIRECTORY
).lines.map(&:strip).reject(&:empty?).reject { |path| File.basename(path) == "README.md" }
records = (current_records + historical_records).uniq.sort
abort "stable release requires evidence records under docs/verification" if records.empty?

first_parent_commits = git_output(
  "rev-list", "--first-parent", "--reverse", release_commit
).lines.map(&:strip).reject(&:empty?)
first_parent_order = first_parent_commits.each_with_index.to_h

ordered_records = records.map do |path|
  unless path.match?(EVIDENCE_PATH_PATTERN)
    abort "invalid immutable verification record filename: #{path}"
  end

  additions = git_output(
    "log", "--first-parent", "--reverse", "--diff-filter=A", "--format=%H",
    release_commit, "--", path
  ).lines.map(&:strip).reject(&:empty?)
  unless additions.length == 1
    abort "immutable evidence record must have exactly one first-parent addition: #{path}"
  end
  addition_commit = additions.first
  addition_contents = git_file_if_present(addition_commit, path)
  release_contents = git_file_if_present(release_commit, path)
  abort "immutable evidence record was deleted after first-parent addition: #{path}" unless release_contents
  unless addition_contents == release_contents
    abort "immutable evidence record changed after first-parent addition: #{path}"
  end
  order = first_parent_order.fetch(addition_commit) do
    abort "evidence addition is absent from first-parent history: #{path}"
  end
  [order, addition_commit, path, release_contents]
end.sort_by { |order, _addition_commit, _path, _contents| order }

latest_gates = {}
ordered_records.group_by do |order, addition_commit, _path, _contents|
  [order, addition_commit]
end.each_value do |entries|
  gates_in_commit = {}
  parsed_entries = []

  entries.each do |_order, _addition_commit, path, contents|
    record_metadata = metadata(contents, ["证据目标", "总体结果", "豁免"], path)
    target = record_metadata.fetch("证据目标")
    unless target&.match?(/\A[0-9a-f]{40}\z/)
      abort "#{path}: 证据目标 must be a full lowercase commit SHA"
    end
    filename_sha = File.basename(path, ".md").split("-").last
    unless filename_sha && target.start_with?(filename_sha)
      abort "#{path}: filename suffix must match the leading characters of 证据目标"
    end
    unless system("git", "cat-file", "-e", "#{target}^{commit}", out: File::NULL, err: File::NULL)
      abort "#{path}: evidence target is not a commit: #{target}"
    end
    unless system("git", "merge-base", "--is-ancestor", target, release_commit, out: File::NULL, err: File::NULL)
      abort "#{path}: evidence target #{target} is not an ancestor of the release commit"
    end

    overall = record_metadata.fetch("总体结果")
    unless %w[Passed Failed Partial].include?(overall)
      abort "#{path}: invalid 总体结果 #{overall.inspect}"
    end
    waiver = record_metadata.fetch("豁免")
    unless waiver == "None" || waiver.to_s.length >= 10
      abort "#{path}: 豁免 must be None or a specific waiver of at least 10 characters"
    end

    rows = evidence_rows(contents, path, overall)
    next unless evidence_target_is_current?(target, release_commit, path)

    rows.each_key do |gate_id|
      if gates_in_commit.key?(gate_id)
        abort "ambiguous latest evidence for #{gate_id}: #{gates_in_commit.fetch(gate_id)} and #{path} were added in the same first-parent commit"
      end
      gates_in_commit[gate_id] = path
    end
    parsed_entries << [path, rows, waiver]
  end

  parsed_entries.each do |path, rows, waiver|
    rows.each do |gate_id, result|
      latest_gates[gate_id] = { result: result, waiver: waiver, path: path }
    end
  end
end

passed_gates = latest_gates.select do |_gate_id, result|
  result.fetch(:result) == "Passed" && result.fetch(:waiver) == "None"
end
missing = required_gates - passed_gates.keys
unless missing.empty?
  abort "stable release is missing Passed evidence in latest unwaived results for: #{missing.join(', ')}"
end

puts "validated #{required_gates.length} stable release Gates across #{passed_gates.values.map { |result| result.fetch(:path) }.uniq.length} latest evidence record(s)"
