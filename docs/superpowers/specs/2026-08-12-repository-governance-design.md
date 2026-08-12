# Repository Governance Design

**Date:** 2026-08-12
**Status:** Approved
**Scope:** Public contribution guidance, private security reporting, repository security settings, and lightweight `main` branch protection

## Context

Codex Mobile Bridge has a public MIT-licensed repository, a primary maintainer, pull-request history, CI release gates, and a verifiable beta release. Its GitHub Community Profile currently recognizes only the license and README, while contributor and security entry points are missing. The repository is maintained by one person, so governance must improve trust and contribution quality without introducing approval rules that the maintainer cannot satisfy.

## Goals

- Give contributors clear paths for bugs, feature proposals, and pull requests.
- Keep undisclosed vulnerabilities and credentials out of public issues.
- Make contribution requirements reflect the repository's real architecture, validation commands, release process, and security boundaries.
- Enable practical GitHub security controls.
- Protect `main` with the existing pull-request release gate without blocking a single-maintainer workflow.
- Improve GitHub's machine-readable Community Profile and the public maintainer signal used by reviewers.

## Non-goals

- Creating a formal multi-person governance body or approval hierarchy.
- Publishing a personal security email address.
- Promising response or remediation service-level agreements that the project cannot guarantee.
- Adding scheduled Dependabot version-update pull requests.
- Requiring signed commits or an approving review at the branch-protection layer.
- Adding a code of conduct, support policy, GitHub Discussions, or other processes that do not yet have an active community need.

## Governance Files

Governance documents use English as the canonical text. The most important security-reporting, redaction, and validation instructions also include concise Simplified Chinese guidance. This keeps one maintainable source of truth while remaining accessible to the repository's current Chinese-speaking users.

### `SECURITY.md`

The security policy will:

- Cover the latest GitHub Release and the current `main` branch. Older releases are unsupported; reporters should confirm the issue against a supported version where practical.
- Direct all suspected vulnerabilities to GitHub Private vulnerability reporting at the repository's Security Advisories page.
- Explicitly prohibit disclosure through public issues, discussions, pull requests, or other public channels before remediation.
- Ask reporters for affected version, platform, impact, reproduction conditions, and a minimal proof of concept.
- Require redaction of pairing tokens, device sessions, local control tokens, Cloudflare Tunnel tokens, VAPID keys, API keys, fixed private domains, full local paths, and private conversation content.
- Commit only to acknowledging and assessing reports as maintainer capacity permits. It will not state a fixed response or remediation SLA.
- Explain coordinated disclosure: the maintainer and reporter should agree on public disclosure after a fix or mitigation is available.

### `CONTRIBUTING.md`

The contribution guide will:

- Ask contributors to search existing issues before opening a report.
- Allow focused fixes to proceed as pull requests while requiring an issue or design discussion before large behavior, protocol, security, architecture, or dependency changes.
- Explain the repository structure and the product boundary: the Mac remains the execution environment; the phone PWA does not directly call model APIs.
- Use fork or topic-branch pull requests and clear Conventional Commit-style messages.
- Define validation by change scope using the commands already maintained in `AGENTS.md` and the repository scripts.
- Require README updates for user-visible changes and `AGENTS.md` updates for architecture, security invariants, validation, or release-process changes.
- Forbid credentials, real pairing links, private tunnel domains, unredacted diagnostics, generated bundles, `target/`, and `node_modules/`.
- State that user-visible changes included in the DMG require the repository's version-sync policy, while documentation-only and test-only changes do not automatically require a version bump.

### Issue forms

`.github/ISSUE_TEMPLATE/bug_report.yml` will collect:

- Product version, macOS version and CPU architecture, and ChatGPT/Codex Desktop version.
- Access mode: LAN, Named Tunnel, Quick Tunnel, or not applicable.
- Problem description, reproduction steps, expected behavior, and actual behavior.
- Relevant, redacted logs or diagnostics.
- Confirmation that the report does not disclose an unpatched vulnerability or secrets.

`.github/ISSUE_TEMPLATE/feature_request.yml` will collect:

- User problem and use case rather than only a proposed implementation.
- Current limitation, desired outcome, alternatives considered, and additional context.
- Affected area: Desktop shell, Bridge/core, mobile PWA, tunnel, notifications, release/tooling, or other.

`.github/ISSUE_TEMPLATE/config.yml` will:

- Disable blank issues.
- Link contributors to the README and contribution guide.
- Link suspected vulnerabilities to the private Security Advisories reporting page.

### Pull-request template

`.github/pull_request_template.md` will use five compact sections:

- Why
- What changed
- Validation
- Security and privacy
- Release impact

Its checklist will require contributors to identify applicable tests, documentation changes, security impact, and version impact. It will not require irrelevant boxes to be checked; contributors may mark items not applicable and explain why.

## GitHub Security Settings

The repository will enable:

- Private vulnerability reporting.
- Dependabot alerts.
- Automated security fixes.

Scheduled Dependabot version updates are intentionally excluded. The project can add them later when it has the capacity to triage routine upgrade pull requests and appropriate language-specific grouping rules.

## `main` Branch Protection

The `main` branch will require:

- Changes to arrive through a pull request.
- The existing `Release gates / release-gate` status check to pass.
- Branches to be up to date before merging so checks run against the current base.
- Pull-request conversations to be resolved before merging.
- Force pushes and branch deletion to remain disabled.

The rule will not require approving reviews because the repository currently has one maintainer. It will not require signed commits because GitHub-generated merge commits and external contributors may not satisfy that rule even though the maintainer's local commits are signed.

The repository administrator will have an explicit emergency bypass in the GitHub rule configuration. Routine maintainer work must still use a pull request and pass the required check; the bypass exists only to recover from a broken rule or unavailable required check. The implementation will prefer a repository ruleset when the API supports this combination and will verify the effective bypass and enforcement fields after creation.

## Delivery Sequence

1. Add the approved governance files on a dedicated branch.
2. Validate YAML syntax, Markdown formatting, links, and `git diff --check`.
3. Push the signed maintainer commit and open a pull request.
4. Wait for `Release gates / release-gate` to pass, then merge the pull request.
5. Enable Private vulnerability reporting, Dependabot alerts, and automated security fixes.
6. Configure `main` branch protection using the exact successful status-check context reported by GitHub.
7. Query GitHub's APIs to verify the Community Profile, security settings, branch protection, public files, and commit verification state.

The branch rule is applied after the governance pull request is merged. This prevents an incorrectly named required check from blocking the first governance delivery and lets implementation resolve the exact status-check identity from a real successful run.

## Validation and Acceptance Criteria

Implementation is complete when:

- GitHub Community Profile recognizes the contribution guide, security policy, issue template, and pull-request template.
- The public repository renders each governance file and issue form without schema errors.
- The private vulnerability reporting endpoint is enabled and linked from the issue chooser.
- Dependabot alerts and automated security fixes report enabled.
- `main` requires pull requests, the correct release-gate check, up-to-date branches, and resolved conversations; force push and deletion are disabled.
- The governance pull request's maintainer-authored commit is signed and GitHub reports it as verified.
- No personal email, credential, real pairing URL, private domain, or unredacted diagnostic data is introduced.

## Risks and Mitigations

- **Incorrect required-check name locks merges:** resolve the check context from the completed governance PR before applying branch protection; retain repository-owner emergency bypass.
- **Security report link is public instead of private:** use the repository's `/security/advisories/new` route and verify Private vulnerability reporting before advertising it as operational.
- **Templates become ceremonial:** keep fields limited to information used for triage and align validation instructions with actual repository scripts.
- **Automated fixes introduce unsafe dependency changes:** automated fixes still arrive as pull requests and must pass the same review and release gates; they are not auto-merged.
- **Single-maintainer bottleneck:** omit mandatory approving reviews and fixed SLA promises until the maintainer base grows.
