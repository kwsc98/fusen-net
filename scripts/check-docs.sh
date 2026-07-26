#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0 OR MIT

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

ruby <<'RUBY'
require "pathname"
require "uri"
require File.expand_path("scripts/docs_metadata", Dir.pwd)

root = Pathname.pwd.expand_path
failures = []
markdown_files = `git ls-files --cached --others --exclude-standard -z -- '*.md'`.split("\0")
first_party = markdown_files.reject { |path| path.start_with?("vendor/") }
link_pattern = /!?\[[^\]]*\]\(\s*(?:<([^>]+)>|([^\s)]+))/

markdown_files.each do |source|
  File.read(source).scan(link_pattern) do |bracketed, plain|
    target = bracketed || plain
    next if target.empty? || target.start_with?("#")
    next if target.match?(/\A(?:[a-z][a-z0-9+.-]*:|\/\/)/i)

    encoded_path = target.split("#", 2).first.split("?", 2).first
    next if encoded_path.empty?

    begin
      relative_path = URI::DEFAULT_PARSER.unescape(encoded_path)
    rescue URI::InvalidURIError
      failures << "#{source}: invalid local link #{target.inspect}"
      next
    end

    destination = Pathname.new(source).dirname.join(relative_path).expand_path
    unless destination == root || destination.to_s.start_with?("#{root}#{File::SEPARATOR}")
      failures << "#{source}: local link escapes the repository: #{target}"
      next
    end
    failures << "#{source}: missing local link target #{target}" unless destination.exist?
  end
end

first_party.each do |source|
  File.foreach(source).with_index(1) do |line, number|
    failures << "#{source}:#{number}: trailing whitespace" if line.match?(/[ \t]+(?:\r?\n)?\z/)
  end
  failures << "#{source}: contains the retired project name" if File.read(source).match?(/fusen(?:-net)?/i)
end

state_fields = {
  "文档适用性" => ["Current", "Proposed", "Conditional", "Historical"],
  "设计评审状态" => ["Draft", "Approved", "Rejected", "Superseded", "N/A"],
  "ADR 决策状态" => ["Proposed", "Accepted", "Rejected", "Superseded", "N/A"],
  "交付状态" => ["Not started", "In progress", "Implemented", "N/A"],
  "验证状态" => ["Unverified", "Partially verified", "Verified", "N/A"],
  "发布状态" => ["Unreleased", "Prerelease", "Stable", "N/A"]
}

adr_path_pattern = %r{\Adocs/adr/\d{4}-[A-Za-z0-9._-]+\.md\z}
evidence_path_pattern = %r{\Adocs/verification/\d{4}-\d{2}-\d{2}-[A-Za-z0-9._-]+-[0-9a-f]{7,40}\.md\z}
atomic_gate_pattern = /\A`([A-Z][A-Z0-9]*(?:-[A-Z0-9]+)+-\d{2})`\z/
evidence_header = ["Gate ID", "命令或场景", "预期", "实际", "结果", "Artifact / hash"]
release_status_projections = {
  "docs/README.md" => "documentation-status",
  "docs/adr/README.md" => "adr-status",
  "docs/compatibility.md" => %w[
    support-status
    protocol-status
    transport-status
    platform-status
  ],
  "docs/deployment.md" => "deployment-status",
  "docs/design-overview.md" => "design-status",
  "docs/releasing.md" => "release-status",
  "docs/roadmap.md" => "roadmap-status",
  "docs/verification/README.md" => "verification-status"
}.freeze
release_status_marker_pattern = /\A<!-- stellaris-release-status:([a-z0-9]+(?:-[a-z0-9]+)*):(start|end) -->\z/
release_gate_pattern = /[A-Z][A-Z0-9]*(?:-[A-Z0-9]+)+-\d{2}/

def release_status_projection_failures(contents, source, expected_ids, marker_pattern, gate_pattern)
  failures = []
  expected_ids = Array(expected_ids)
  active_id = nil
  block = []
  seen_ids = {}
  in_fence = false

  contents.lines.each_with_index do |line, index|
    stripped = line.strip
    marker = stripped.match(marker_pattern)
    unless active_id
      if stripped.start_with?("```") || stripped.start_with?("~~~")
        in_fence = !in_fence
        next
      end
      next if in_fence
    end

    if stripped.include?("stellaris-release-status:") && marker.nil?
      failures << "#{source}:#{index + 1}: malformed release-status marker"
    end

    unless marker
      block << line if active_id
      next
    end

    projection_id = marker[1]
    boundary = marker[2]
    unless expected_ids.include?(projection_id)
      failures << "#{source}:#{index + 1}: release-status projection #{projection_id.inspect} is not allowlisted for this path"
    end

    if boundary == "start"
      if active_id
        failures << "#{source}:#{index + 1}: nested release-status projection"
        next
      end
      if seen_ids.key?(projection_id)
        failures << "#{source}:#{index + 1}: duplicate release-status projection #{projection_id}"
      end
      active_id = projection_id
      block = []
      next
    end

    unless active_id == projection_id
      failures << "#{source}:#{index + 1}: unmatched release-status end marker for #{projection_id}"
      next
    end

    projection = block.join
    failures << "#{source}: release-status projection #{projection_id} is empty" if projection.strip.empty?
    if projection.match?(gate_pattern)
      failures << "#{source}: release-status projection #{projection_id} may not contain Gate IDs"
    end
    if projection.lines.any? { |projection_line| projection_line.match?(/^\s*\#{1,6}\s/) }
      failures << "#{source}: release-status projection #{projection_id} may not contain headings"
    end

    seen_ids[projection_id] = true
    active_id = nil
    block = []
  end

  failures << "#{source}: missing release-status end marker for #{active_id}" if active_id
  expected_ids.each do |expected_id|
    unless seen_ids.key?(expected_id)
      failures << "#{source}: missing required release-status projection #{expected_id}"
    end
  end
  failures
end

def parse_evidence_record(contents, source, evidence_header, atomic_gate_pattern)
  if contents.match?(/<!--|-->|```|~~~|<\/?[A-Za-z][^>]*>/)
    raise DocsMetadata::Error, "#{source}: may not hide evidence in HTML, comments, or fenced code blocks"
  end
  metadata = DocsMetadata.read(contents, ["证据目标", "总体结果", "豁免"])
  target = metadata.fetch("证据目标")
  unless target&.match?(/\A[0-9a-f]{40}\z/)
    raise DocsMetadata::Error, "#{source}: 证据目标 must be one full lowercase 40-character commit SHA"
  end
  overall = metadata.fetch("总体结果")
  unless %w[Passed Failed Partial].include?(overall)
    raise DocsMetadata::Error, "#{source}: invalid 总体结果 #{overall.inspect}"
  end
  waiver = metadata.fetch("豁免")
  unless waiver == "None" || waiver.to_s.length >= 10
    raise DocsMetadata::Error, "#{source}: 豁免 must be None or a specific waiver of at least 10 characters"
  end

  lines = contents.lines
  header_indexes = lines.each_index.select do |index|
    lines[index].strip.split("|").map(&:strip).reject(&:empty?) == evidence_header
  end
  unless header_indexes.length == 1
    raise DocsMetadata::Error, "#{source}: requires exactly one canonical Gate evidence table"
  end

  index = header_indexes.first
  separators = lines[index + 1].to_s.strip.split("|").map(&:strip).reject(&:empty?)
  unless separators.length == evidence_header.length && separators.all? { |cell| cell.match?(/\A:?-{3,}:?\z/) }
    raise DocsMetadata::Error, "#{source}: invalid Gate evidence table separator"
  end

  rows = {}
  lines.drop(index + 2).each do |line|
    break unless line.lstrip.start_with?("|")
    cells = line.strip.sub(/\A\|/, "").sub(/\|\z/, "").split("|", -1).map(&:strip)
    raise DocsMetadata::Error, "#{source}: Gate evidence row must have six cells" unless cells.length == 6
    gate_match = cells[0].match(atomic_gate_pattern)
    raise DocsMetadata::Error, "#{source}: invalid Gate ID #{cells[0].inspect}" unless gate_match
    gate_id = gate_match[1]
    raise DocsMetadata::Error, "#{source}: duplicate Gate ID #{gate_id}" if rows.key?(gate_id)
    if cells.values_at(1, 2, 3, 5).any?(&:empty?)
      raise DocsMetadata::Error, "#{source}: Gate #{gate_id} contains an empty evidence cell"
    end
    result = cells[4]
    unless %w[Passed Failed Partial].include?(result)
      raise DocsMetadata::Error, "#{source}: Gate #{gate_id} has invalid result #{result.inspect}"
    end
    unless cells[5].match?(/\bsha256:[0-9a-f]{64}\b/)
      raise DocsMetadata::Error, "#{source}: Gate #{gate_id} Artifact / hash must contain sha256:<64 lowercase hex>"
    end
    rows[gate_id] = result
  end
  raise DocsMetadata::Error, "#{source}: Gate evidence table is empty" if rows.empty?

  expected = if rows.value?("Failed")
               "Failed"
             elsif rows.value?("Partial")
               "Partial"
             else
               "Passed"
             end
  unless overall == expected
    raise DocsMetadata::Error, "#{source}: 总体结果 #{overall} does not match row results #{expected}"
  end
  [target, rows, waiver]
end

first_party.each do |source|
  failures.concat(
    release_status_projection_failures(
      File.read(source),
      source,
      release_status_projections[source],
      release_status_marker_pattern,
      release_gate_pattern
    )
  )
end

Dir.glob("docs/**/*.md").sort.each do |source|
  contents = File.read(source)
  labels = state_fields.keys + ["适用范围", "设计批准", "关联 ADR"]
  begin
    metadata = DocsMetadata.read(contents, labels)
  rescue DocsMetadata::Error => error
    failures << "#{source}: #{error.message}"
    next
  end
  state_fields.each do |label, allowed|
    value = metadata.fetch(label)
    if value.nil?
      failures << "#{source}: missing #{label} metadata near the top"
      next
    end
    unless allowed.include?(value)
      failures << "#{source}: invalid #{label} #{value.inspect}; expected #{allowed.join(', ')}"
    end
  end

  failures << "#{source}: missing 适用范围 metadata near the top" if metadata.fetch("适用范围").nil?

  design_status = metadata.fetch("设计评审状态")
  if %w[Draft Approved Rejected Superseded].include?(design_status)
    approval = metadata.fetch("设计批准")
    failures << "#{source}: missing 设计批准 metadata near the top" if approval.nil? || approval.empty?
    if design_status == "Approved"
      approval_sha = approval.to_s[/\b[0-9a-f]{40}\b/i]
      if approval_sha.nil?
        failures << "#{source}: Approved design must record a full approval commit SHA"
      elsif !system("git", "cat-file", "-e", "#{approval_sha}^{commit}", out: File::NULL, err: File::NULL)
        failures << "#{source}: design approval SHA is not a local commit: #{approval_sha}"
      elsif !system("git", "merge-base", "--is-ancestor", approval_sha, "HEAD", out: File::NULL, err: File::NULL)
        failures << "#{source}: design approval commit is not an ancestor of HEAD: #{approval_sha}"
      end
    end

    related_adrs = metadata.fetch("关联 ADR")
    if related_adrs.nil?
      failures << "#{source}: missing 关联 ADR metadata near the top"
    elsif related_adrs.match?(/\AN\/?A[：:]\s*.{10,}\z/i)
      # D2 designs explicitly record why no authoritative ADR trigger applies.
      unless metadata.fetch("ADR 决策状态") == "N/A"
        failures << "#{source}: N/A 关联 ADR requires ADR 决策状态: N/A"
      end
    else
      paths = related_adrs.scan(%r{docs/[A-Za-z0-9._/-]+\.md}).uniq
      if paths.empty? || !paths.all? { |path| path.match?(adr_path_pattern) } || related_adrs != paths.join(", ")
        failures << "#{source}: 关联 ADR must be N/A: <specific reason> or a canonical comma-separated ADR path list"
      else
        associated_statuses = []
        paths.each do |adr_path|
          unless File.file?(adr_path)
            failures << "#{source}: associated ADR does not exist: #{adr_path}"
            next
          end
          begin
            adr_status = DocsMetadata.read(File.read(adr_path), ["ADR 决策状态"]).fetch("ADR 决策状态")
            associated_statuses << adr_status
            if design_status == "Approved" && adr_status != "Accepted"
              failures << "#{source}: Approved design requires Accepted ADR: #{adr_path}"
            end
          rescue DocsMetadata::Error => error
            failures << "#{adr_path}: #{error.message}"
          end
        end
        if associated_statuses.uniq.length > 1
          failures << "#{source}: associated ADRs must share one decision status"
        elsif associated_statuses.length == paths.length &&
            metadata.fetch("ADR 决策状态") != associated_statuses.first
          failures << "#{source}: ADR 决策状态 must match associated ADR records"
        end
      end
    end
  end
end


Dir.glob("docs/verification/*.md").sort.each do |source|
  next if source == "docs/verification/README.md"
  unless source.match?(evidence_path_pattern)
    failures << "#{source}: invalid verification record filename"
    next
  end
  begin
    target, _rows, _waiver = parse_evidence_record(File.read(source), source, evidence_header, atomic_gate_pattern)
    filename_sha = File.basename(source, ".md").split("-").last
    failures << "#{source}: filename suffix does not match 证据目标" unless target.start_with?(filename_sha)
    unless system("git", "cat-file", "-e", "#{target}^{commit}", out: File::NULL, err: File::NULL)
      failures << "#{source}: 证据目标 is not a local commit: #{target}"
    else
      unless system("git", "merge-base", "--is-ancestor", target, "HEAD", out: File::NULL, err: File::NULL)
        failures << "#{source}: 证据目标 is not an ancestor of HEAD: #{target}"
      end
    end
  rescue DocsMetadata::Error => error
    failures << error.message
  end
end

abort failures.join("\n") unless failures.empty?
puts "validated #{markdown_files.length} Markdown files (#{first_party.length} first-party)"
RUBY
