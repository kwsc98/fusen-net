#!/usr/bin/env ruby
# SPDX-License-Identifier: Apache-2.0 OR MIT

require "yaml"

WORKFLOW_PATH = File.expand_path("../.github/workflows/release.yml", __dir__)

def assert(condition, message)
  abort message unless condition
end

def needs(job)
  Array(job.fetch("needs"))
end

workflow = YAML.load_file(WORKFLOW_PATH)
assert(workflow.is_a?(Hash), "release workflow must parse as a mapping")

concurrency = workflow.fetch("concurrency")
assert(
  concurrency.fetch("group") == "release-${{ inputs.tag || github.ref_name }}",
  "release concurrency must be scoped to the requested tag"
)
assert(
  concurrency.fetch("cancel-in-progress") == false,
  "release concurrency must queue, not cancel, a run for the same tag"
)

jobs = workflow.fetch("jobs")
preflight = jobs.fetch("publication-preflight")
assert(
  needs(preflight).sort == %w[build release-gate],
  "publication preflight must run after qualification and all archive builds"
)
assert(
  preflight.fetch("permissions").fetch("packages") == "read",
  "publication preflight requires read-only package access"
)
assert(
  preflight.fetch("permissions").fetch("contents") == "write",
  "publication preflight requires draft GitHub Release visibility"
)

preflight_script = preflight.fetch("steps").map { |step| step["run"] }.compact.join("\n")
[
  "releases/tags/${RELEASE_TAG}",
  "for component in server agent",
  "scope=repository:${image_repository}:pull",
  "manifests/${RELEASE_TAG}",
  'case "$release_status" in',
  'case "$manifest_status" in',
  "returned unexpected HTTP"
].each do |fragment|
  assert(
    preflight_script.include?(fragment),
    "publication preflight is missing fail-closed check fragment: #{fragment}"
  )
end
assert(
  preflight_script.scan(/^\s*404\)$/).length >= 2 &&
    preflight_script.scan(/^\s*200\)$/).length >= 2,
  "publication preflight must distinguish absent and existing Release/GHCR destinations"
)

container = jobs.fetch("container")
github_release = jobs.fetch("github-release")
assert(
  needs(container).include?("publication-preflight"),
  "container publication must depend on publication preflight"
)
assert(
  needs(github_release).include?("publication-preflight") &&
    needs(github_release).include?("container"),
  "GitHub Release publication must depend on preflight and container publication"
)

container_pushes = container.fetch("steps").select do |step|
  step.fetch("uses", "").start_with?("docker/build-push-action@") &&
    step.fetch("with", {}).fetch("push", false) == true
end
assert(container_pushes.length == 2, "release workflow must publish exactly two checked GHCR images")

release_steps = github_release.fetch("steps")
recheck = release_steps.find { |step| step["name"] == "Reconfirm the GitHub Release is still unused" }
assert(recheck, "GitHub Release publication requires an immediate absence recheck")
assert(
  recheck.fetch("run").include?('test "$status" = 404'),
  "GitHub Release recheck must accept only an explicit 404"
)

publisher = release_steps.find do |step|
  step["name"] == "Create and publish a new GitHub Release"
end
assert(publisher, "release workflow must contain the GitHub Release publisher")
assert(
  release_steps.none? { |step| step.fetch("uses", "").start_with?("softprops/action-gh-release@") },
  "GitHub Release publication must not use an update-or-create action"
)

publisher_script = publisher.fetch("run")
[
  "--rawfile body release-body.md",
  '"${GITHUB_API_URL}/repos/${GITHUB_REPOSITORY}/releases"',
  'if [[ "$create_status" != 201 ]]',
  'release_id="$(',
  'select(.tag_name == $tag and .draft == true)',
  '.upload_url',
  '| sub(',
  'expected_upload_suffix="/releases/${release_id}/assets"',
  "'$name | @uri'",
  'find dist -maxdepth 1 -type f -print0 | sort -z > "$asset_list"',
  '--header "Content-Type: ${content_type}"',
  '--data-binary "@${asset_path}"',
  'if [[ "$upload_status" != 201 ]]',
  'releases/${release_id}/assets?per_page=100',
  'length == $count',
  '--request PATCH',
  'releases/${release_id}"',
  'if [[ "$finalize_status" != 200 ]]'
].each do |fragment|
  assert(
    publisher_script.include?(fragment),
    "create-only GitHub Release publisher is missing fail-closed fragment: #{fragment}"
  )
end
assert(
  release_steps.index(recheck) + 1 == release_steps.index(publisher),
  "the absence recheck must immediately precede create-only publication"
)
assert(
  publisher_script.scan(%r{/repos/\$\{GITHUB_REPOSITORY\}/releases/\$\{release_id\}}).length >= 2,
  "asset verification and finalization must use the ID returned by release creation"
)
assert(
  !publisher_script.include?("overwrite_files") &&
    !publisher_script.include?("releases/tags/${RELEASE_TAG}"),
  "the publisher must never look up or update a pre-existing release"
)

puts "validated release workflow concurrency and create-only immutable publication"
