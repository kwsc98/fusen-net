# SPDX-License-Identifier: Apache-2.0 OR MIT

param(
    [ValidateSet("standard", "all")]
    [string]$Scenario = "standard",
    [switch]$RequireFullOverlay
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
$isAdmin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    throw "The Windows real TUN runner must have administrator privileges."
}

if (-not (Test-Path Cargo.lock -PathType Leaf)) {
    throw "Cargo.lock must be committed."
}
cargo test `
    --package fusen-net `
    --test real_tun `
    --all-features `
    --locked `
    -- `
    --exact native_tun_protocol_and_route_lifecycle `
    --ignored `
    --nocapture `
    --test-threads=1
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
if ($RequireFullOverlay) {
    throw "Windows cross-host Relay/Edge orchestration is not implemented; stable qualification remains blocked."
}
