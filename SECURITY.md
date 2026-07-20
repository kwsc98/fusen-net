# Security Policy

Fusen Net is networking software that handles authentication material and
untrusted packets. Please report suspected vulnerabilities privately, even
when you are unsure whether the behavior is exploitable.

## Supported versions

Until 0.1 reaches a stable release, security fixes are made only on `main` and
the latest published 0.1 prerelease.

| Version | Security updates |
| --- | --- |
| `main` / latest `0.1.x` prerelease | Yes |
| Older prereleases and `0.0.x` | No |
| Legacy TCP branches or images | No |

Users of an unsupported version should first reproduce with the latest
supported version when it is safe to do so. The support policy may change at
the stable 0.1 release and will be recorded here and in the release notes.

## Reporting a vulnerability

Use GitHub Private Vulnerability Reporting:

<https://github.com/kwsc98/fusen-net/security/advisories/new>

Do not open a public Issue, Discussion, or Pull Request for an undisclosed
vulnerability. Do not include live private keys, tokens, packet captures with
user data, or credentials from systems you do not own. Revoke any credential
that may have been exposed before sharing a minimal replacement fixture.

A useful report includes:

- affected commit, release, platform, and QUIC backend;
- impact and the trust boundary that is crossed;
- minimal reproduction steps or a small proof of concept;
- whether the issue requires a valid node token or elevated local privileges;
- suggested mitigation, if known;
- your preferred name for credit, or a request to remain anonymous.

The maintainers aim to acknowledge a report within 3 business days and provide
an initial triage result within 7 business days. Fix and disclosure timing
depends on severity, affected dependencies, and release validation. These are
targets, not a paid support SLA.

## Coordinated disclosure

After validation, maintainers will agree on scope, severity, mitigation, fix,
tests, advisory text, and a disclosure date with the reporter. Please keep the
issue private until a fixed release and advisory are available, or until an
agreed deadline has passed. The project will credit reporters who request it.

Security releases should include a GitHub advisory, affected/fixed versions,
upgrade or mitigation instructions, and updated checksums/provenance. Existing
release artifacts and tags are never silently replaced.

## Safe harbor

The project will not pursue action against good-faith research that:

- tests only systems and accounts the researcher owns or is authorized to use;
- avoids privacy violations, service degradation, persistence, and data loss;
- uses the minimum access needed to demonstrate the issue;
- reports promptly through the private channel and allows reasonable time to
  remediate; and
- follows applicable law.

This policy does not authorize testing third-party deployments or infrastructure
and does not create a bug bounty or promise compensation.

For deployment assumptions and known non-goals, see
[`docs/security-model.md`](docs/security-model.md).
