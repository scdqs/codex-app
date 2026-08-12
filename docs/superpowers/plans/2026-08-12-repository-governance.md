# Repository Governance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a practical public contribution and security baseline, enable GitHub security controls, and protect `main` with the repository's existing release gate without blocking its single maintainer.

**Architecture:** Governance is split between versioned repository files and auditable GitHub settings. Documentation and forms are delivered first through a signed pull request; after its `release-gate` check passes and it is merged, repository security controls and a branch ruleset are enabled and verified through GitHub's APIs.

**Tech Stack:** GitHub Markdown, GitHub Issue Forms YAML, GitHub Actions, GitHub REST API, GitHub CLI, Git SSH commit signing

## Global Constraints

- English is the canonical governance language; critical security-reporting, redaction, and validation instructions include concise Simplified Chinese guidance.
- Security reports use GitHub Private vulnerability reporting; do not publish a personal security email.
- Do not promise a fixed acknowledgement or remediation SLA.
- Do not require approving reviews or signed commits in the branch ruleset.
- Do not add scheduled Dependabot version-update pull requests.
- Do not add `CODE_OF_CONDUCT.md`, `SUPPORT.md`, Discussions, or unrelated governance processes.
- Do not include credentials, real pairing links, private tunnel domains, private conversation content, full local paths from diagnostics, or generated application artifacts.
- Documentation-only governance changes do not bump `VERSION`.
- Preserve all user changes in `/Users/damon/Documents/my_ai/codex-app`; perform implementation only in `/tmp/codex-app-governance.jTeAkH` or a fresh isolated worktree based on `origin/main`.
- All maintainer-authored commits use `Damon <5986625+scdqs@users.noreply.github.com>` and the registered SSH signing key.

---

## File Map

- `SECURITY.md`: Supported versions, private reporting route, required report contents, redaction rules, and coordinated disclosure expectations.
- `CONTRIBUTING.md`: Contribution workflow, repository boundaries, validation matrix, documentation/version rules, and secret-handling requirements.
- `.github/ISSUE_TEMPLATE/bug_report.yml`: Structured bug intake with platform, version, access mode, reproduction, diagnostics, and security confirmation.
- `.github/ISSUE_TEMPLATE/feature_request.yml`: Structured feature intake centered on user problems, outcomes, alternatives, and affected components.
- `.github/ISSUE_TEMPLATE/config.yml`: Disable blank issues and route documentation and security requests.
- `.github/pull_request_template.md`: Compact PR rationale, changes, validation, security/privacy, release impact, and checklist.
- `docs/superpowers/specs/2026-08-12-repository-governance-design.md`: Change status from pending review to approved.
- `/tmp/codex-main-ruleset.json`: Ephemeral GitHub ruleset request payload; never commit it.

### Task 1: Add the security and contribution policies

**Files:**
- Create: `SECURITY.md`
- Create: `CONTRIBUTING.md`
- Modify: `docs/superpowers/specs/2026-08-12-repository-governance-design.md`

**Interfaces:**
- Consumes: Existing product boundaries and validation commands from `AGENTS.md`; version/release behavior from `README.md` and `docs/release-gates.md`.
- Produces: Public contribution and security URLs consumed by the Issue chooser and Community Profile API.

- [ ] **Step 1: Confirm the implementation branch is isolated and current**

Run:

```bash
git status --short --branch
git fetch origin main
git merge-base --is-ancestor origin/main HEAD
```

Expected: only the approved spec/plan commits are ahead of `origin/main`; no unrelated working-tree changes exist. If `origin/main` advanced, rebase the governance branch before creating implementation files.

- [ ] **Step 2: Create `SECURITY.md` with this exact policy**

```markdown
# Security Policy

## Supported versions

Security fixes are developed against the current `main` branch and, when applicable, released in the latest GitHub Release. Older releases are not supported. Before reporting, please confirm the issue against `main` or the latest release when it is safe and practical to do so.

## Report a vulnerability privately

Please use [GitHub Private vulnerability reporting](https://github.com/scdqs/codex-app/security/advisories/new) for suspected vulnerabilities.

Do **not** disclose an unpatched vulnerability through a public issue, pull request, discussion, or other public channel. Do not include credentials or private user data in any public report.

请通过 GitHub Private vulnerability reporting 私密报告安全问题。修复完成前，请勿在公开 Issue、PR 或其他公开渠道披露漏洞，也不要提交任何凭据或私人数据。

Include, where available:

- The affected release or commit.
- macOS version and CPU architecture.
- A concise impact assessment and attack prerequisites.
- Reproduction steps or a minimal proof of concept.
- Suggested mitigations or fixes, if known.
- Whether the issue is already public or known to others.

## Protect secrets and user data

Redact pairing tokens, device sessions, Local Control Tokens, Cloudflare Tunnel tokens, VAPID keys, API keys, fixed private domains, full local paths, and private conversation content. Prefer the app's redacted diagnostics and reduce any proof of concept to the minimum data needed to reproduce the issue.

提交日志或诊断前，请移除配对令牌、设备会话、Local Control Token、Cloudflare Tunnel Token、VAPID/API 密钥、私人域名、完整本机路径和私人会话内容。

## What to expect

The maintainer will acknowledge and assess reports as capacity permits. Response and remediation time depend on severity, reproducibility, and release constraints; this project does not promise a fixed service-level agreement.

Please allow time for a fix or mitigation before public disclosure. When appropriate, the maintainer will coordinate an advisory, release, credit, and disclosure timing with the reporter.
```

- [ ] **Step 3: Create `CONTRIBUTING.md` with this exact guide**

```markdown
# Contributing to Codex Mobile Bridge

Thanks for helping improve Codex Mobile Bridge. The project connects a Mac running ChatGPT/Codex Desktop to a paired phone PWA, so changes must preserve local security boundaries and predictable release behavior.

## Before you start

- Search existing issues before opening a new bug or feature request.
- Focused fixes may go directly to a pull request.
- Open an issue before large behavior, protocol, dependency, architecture, security, or release-process changes so scope can be agreed first.
- Suspected vulnerabilities must use [Private vulnerability reporting](https://github.com/scdqs/codex-app/security/advisories/new), not a public issue.

提交前请先搜索已有 Issue。涉及安全漏洞时必须使用 Private vulnerability reporting，不要公开披露。

## Product and repository boundaries

The Mac remains the execution environment. The Bridge reads and writes real ChatGPT/Codex Desktop sessions; the phone PWA does not call model APIs directly.

- `apps/desktop-shell/`: Tauri desktop shell and lifecycle UI.
- `apps/bridge-sidecar/`: Local authenticated HTTP/WebSocket service.
- `apps/mobile-pwa/`: React phone PWA.
- `crates/bridge-core/`: CDP, app-server RPC, pairing, API, storage, and event normalization.
- `crates/desktop-core/`: Process, tunnel, configuration, and diagnostics management.
- `packages/bridge-protocol/`: Shared frontend protocol types.
- `docs/`: Design, QA, and release documentation. Plans do not prove a feature is implemented.

## Development workflow

1. Fork the repository or create a focused topic branch.
2. Keep each pull request limited to one coherent change.
3. Follow existing architecture and naming patterns.
4. Use clear Conventional Commit-style messages such as `fix: ...`, `feat: ...`, `test: ...`, or `docs: ...`.
5. Fill in the pull-request template with the validation actually performed.

Do not push directly to `main`.

## Validation

Run the smallest sufficient checks for the files you changed. For shared protocol, HTTP API, remote-access, or release changes, run the full matrix:

```bash
./scripts/cargo.sh test --workspace
./scripts/cargo.sh clippy -p desktop-shell -- -D warnings

cd apps/mobile-pwa
npm test -- --run
npm run build

cd ../desktop-shell
npm test -- --run
npm run build

cd ../..
./scripts/check-version-sync.sh
git diff --check
```

Documentation and template-only changes require at least:

```bash
git diff --check
./scripts/check-version-sync.sh
```

请根据改动范围执行最小充分验证，并在 PR 中准确记录命令与结果；不要声称未实际执行的测试已经通过。

## Documentation and versioning

- Update `README.md` and `README_EN.md` for user-visible behavior.
- Update `AGENTS.md` when architecture, security invariants, validation commands, or release procedures change.
- `VERSION` is the product version source. User-visible behavior shipped in the DMG requires the repository's version-sync process.
- Documentation-only and test-only changes do not automatically require a version bump.
- Do not describe planned work under `docs/` as already delivered.

## Security and privacy requirements

Never commit:

- Pairing, device-session, Local Control, Cloudflare Tunnel, VAPID, API, or other credentials.
- Real pairing URLs, private fixed domains, or unredacted diagnostic exports.
- Private conversation content or unnecessary full local paths.
- Generated bundles, Tauri resources, `target/`, or `node_modules/`.

Use `example.com` and synthetic data in documentation and tests. Keep REST, WebSocket, image, local-control, and tunnel trust boundaries intact. If a change affects authentication, pairing, diagnostics, remote access, or approval handling, explain the security impact in the pull request.

## Pull requests

A reviewable pull request explains why the change is needed, what changed, how it was validated, its security/privacy impact, and whether it affects a release or version. CI release gates must pass before merge, and all review conversations must be resolved.
```

- [ ] **Step 4: Mark the approved design accurately**

Change the design document header to:

```markdown
**Status:** Approved
```

- [ ] **Step 5: Validate and commit the two policy files**

Run:

```bash
rg -n 'T[B]D|T[O]DO|F[I]XME|P[L]ACEHOLDER' SECURITY.md CONTRIBUTING.md docs/superpowers/specs/2026-08-12-repository-governance-design.md
./scripts/check-version-sync.sh
git diff --check
```

Expected: the placeholder scan returns no matches; version sync and diff checks pass.

Commit:

```bash
git add SECURITY.md CONTRIBUTING.md docs/superpowers/specs/2026-08-12-repository-governance-design.md
git commit -S -m "docs: add security and contribution policies"
```

### Task 2: Add structured contribution templates

**Files:**
- Create: `.github/ISSUE_TEMPLATE/bug_report.yml`
- Create: `.github/ISSUE_TEMPLATE/feature_request.yml`
- Create: `.github/ISSUE_TEMPLATE/config.yml`
- Create: `.github/pull_request_template.md`

**Interfaces:**
- Consumes: `SECURITY.md` and `CONTRIBUTING.md` URLs from Task 1.
- Produces: GitHub Issue chooser entries and the default pull-request body.

- [ ] **Step 1: Create the issue-template directory and bug form**

Create `.github/ISSUE_TEMPLATE/bug_report.yml`:

```yaml
name: Bug report
description: Report a reproducible problem in Codex Mobile Bridge
title: "[Bug]: "
labels:
  - bug
body:
  - type: markdown
    attributes:
      value: |
        Thanks for reporting a bug. Search existing issues first and remove secrets or private data from all logs.

        Suspected vulnerabilities must be reported privately through Security Advisories, not with this form.

        提交前请搜索已有 Issue，并从日志中移除所有凭据和私人数据。安全漏洞请使用 Security Advisories 私密报告。
  - type: input
    id: bridge-version
    attributes:
      label: Codex Mobile Bridge version
      description: Use the version shown in the app or the release tag.
      placeholder: v0.1.22 Beta
    validations:
      required: true
  - type: input
    id: macos
    attributes:
      label: macOS and Mac architecture
      placeholder: macOS 15.6, Apple Silicon arm64
    validations:
      required: true
  - type: input
    id: desktop-version
    attributes:
      label: ChatGPT/Codex Desktop version
      description: Include whether you use ChatGPT.app or the legacy Codex.app.
      placeholder: ChatGPT.app 1.2026.xxx
    validations:
      required: true
  - type: dropdown
    id: access-mode
    attributes:
      label: Access mode
      options:
        - LAN
        - Cloudflare Named Tunnel
        - Quick Tunnel
        - Not applicable
    validations:
      required: true
  - type: textarea
    id: problem
    attributes:
      label: Problem
      description: Describe the user-visible problem and when it occurs.
    validations:
      required: true
  - type: textarea
    id: reproduce
    attributes:
      label: Steps to reproduce
      placeholder: |
        1. Start ...
        2. Connect ...
        3. Observe ...
    validations:
      required: true
  - type: textarea
    id: expected
    attributes:
      label: Expected behavior
    validations:
      required: true
  - type: textarea
    id: actual
    attributes:
      label: Actual behavior
    validations:
      required: true
  - type: textarea
    id: diagnostics
    attributes:
      label: Redacted diagnostics or logs
      description: Remove tokens, private domains, conversation content, and full local paths. Drag files here if needed.
      render: shell
  - type: checkboxes
    id: checks
    attributes:
      label: Safety checks
      options:
        - label: I searched existing issues for this problem.
          required: true
        - label: This report does not publicly disclose an unpatched vulnerability.
          required: true
        - label: I removed credentials and private user data from the report.
          required: true
```

- [ ] **Step 2: Create the feature-request form**

Create `.github/ISSUE_TEMPLATE/feature_request.yml`:

```yaml
name: Feature request
description: Propose an improvement based on a concrete user need
title: "[Feature]: "
labels:
  - enhancement
body:
  - type: markdown
    attributes:
      value: |
        Describe the user problem and desired outcome before proposing implementation details. Search existing issues first.

        请先描述用户问题和期望结果，再补充实现建议；提交前请搜索已有 Issue。
  - type: textarea
    id: use-case
    attributes:
      label: User problem and use case
      description: Who needs this, in what situation, and what are they unable to do today?
    validations:
      required: true
  - type: textarea
    id: outcome
    attributes:
      label: Desired outcome
      description: Describe the observable result, not only a technical solution.
    validations:
      required: true
  - type: dropdown
    id: area
    attributes:
      label: Primary area
      options:
        - Desktop shell
        - Bridge or core protocol
        - Mobile PWA
        - LAN or tunnel access
        - Notifications
        - Release or developer tooling
        - Other
    validations:
      required: true
  - type: textarea
    id: alternatives
    attributes:
      label: Alternatives considered
      description: How do you handle this need today, if at all?
  - type: textarea
    id: context
    attributes:
      label: Additional context
      description: Add examples, screenshots, or constraints. Remove private data first.
  - type: checkboxes
    id: checks
    attributes:
      label: Checks
      options:
        - label: I searched existing issues for this request.
          required: true
        - label: I have not included credentials or private user data.
          required: true
```

- [ ] **Step 3: Configure the Issue chooser**

Create `.github/ISSUE_TEMPLATE/config.yml`:

```yaml
blank_issues_enabled: false
contact_links:
  - name: Documentation and setup guide
    url: https://github.com/scdqs/codex-app#readme
    about: Read setup, security boundaries, and troubleshooting before opening an issue.
  - name: Contributing guide
    url: https://github.com/scdqs/codex-app/blob/main/CONTRIBUTING.md
    about: Review the development workflow and validation requirements.
  - name: Report a security vulnerability privately
    url: https://github.com/scdqs/codex-app/security/advisories/new
    about: Do not disclose suspected vulnerabilities in a public issue.
```

- [ ] **Step 4: Create the pull-request template**

Create `.github/pull_request_template.md`:

```markdown
## Why

<!-- What problem does this solve? Link the issue when one exists. -->

## What changed

<!-- Summarize the focused implementation and important trade-offs. -->

## Validation

<!-- List the exact commands and manual checks you ran, with results. -->

## Security and privacy

<!-- Explain authentication, pairing, diagnostics, local paths, user data, tunnel, or approval impact. Write "No security or privacy impact" only after checking. -->

## Release impact

<!-- State whether VERSION, README.md, README_EN.md, AGENTS.md, release notes, or manual device QA are required. -->

## Checklist

- [ ] The change is focused and follows the existing architecture.
- [ ] I ran the applicable validation and reported it accurately above.
- [ ] I removed credentials, private domains, pairing links, private conversation content, and unredacted diagnostics.
- [ ] I updated user or maintainer documentation where applicable, or explained why it is not needed.
- [ ] I assessed version and release impact, including whether DMG behavior changes.
```

- [ ] **Step 5: Validate YAML schemas and repository formatting**

Use Ruby's built-in YAML parser without installing dependencies:

```bash
ruby -e 'require "yaml"; ARGV.each { |path| YAML.safe_load_file(path, aliases: false); puts "OK #{path}" }' \
  .github/ISSUE_TEMPLATE/bug_report.yml \
  .github/ISSUE_TEMPLATE/feature_request.yml \
  .github/ISSUE_TEMPLATE/config.yml

git diff --check
./scripts/check-version-sync.sh
```

Expected: all three YAML files print `OK`; formatting and version sync pass.

- [ ] **Step 6: Commit the structured templates**

```bash
git add .github/ISSUE_TEMPLATE .github/pull_request_template.md
git commit -S -m "docs: add contribution templates"
```

### Task 3: Deliver and verify the governance pull request

**Files:**
- No new files.

**Interfaces:**
- Consumes: Signed commits and governance files from Tasks 1–2.
- Produces: Merged public governance files and a successful `release-gate` check whose context is used by Task 5.

- [ ] **Step 1: Run final local validation**

```bash
./scripts/check-version-sync.sh
./scripts/check-release-gate.sh --channel beta
git diff --check origin/main...HEAD
git status --short --branch
```

Expected: version sync and release gate pass; diff check is clean; only committed governance changes are ahead of `origin/main`.

- [ ] **Step 2: Verify every new commit contains a valid SSH signature locally**

Create a temporary allowed-signers file containing:

```text
5986625+scdqs@users.noreply.github.com <contents of /Users/damon/.ssh/id_rsa.pub>
```

Run for each governance commit:

```bash
git -c gpg.ssh.allowedSignersFile="$allowed_signers" log origin/main..HEAD --show-signature --format=fuller
```

Expected: every maintainer-authored commit prints `Good "git" signature` for the noreply address.

- [ ] **Step 3: Push the branch and create a pull request**

```bash
git push -u origin docs/repository-governance-design
gh pr create \
  --repo scdqs/codex-app \
  --base main \
  --head docs/repository-governance-design \
  --title "docs: add repository governance baseline" \
  --body-file /tmp/codex-governance-pr.md
```

The PR body must fill the new template and state:

- Adds security and contribution policies plus structured Issue/PR templates.
- Validation: YAML parsing, version sync, beta release gate, and diff check.
- Security: introduces a private reporting route and redaction requirements; contains no secrets.
- Release: documentation-only; no version bump or DMG rebuild.

- [ ] **Step 4: Verify GitHub reports the commits as verified**

```bash
gh api "repos/scdqs/codex-app/commits/$(git rev-parse HEAD)" \
  --jq '{sha,verified:.commit.verification.verified,reason:.commit.verification.reason,author:.commit.author.email}'
```

Expected: `verified` is `true`, `reason` is `valid`, and author is the GitHub noreply address.

- [ ] **Step 5: Wait for the exact PR check and merge only after success**

```bash
gh pr checks <PR_NUMBER> --repo scdqs/codex-app --watch
```

Expected: `release-gate` succeeds. If it fails, inspect with `gh run view <RUN_ID> --log-failed`, fix on the branch, rerun validation, and push another signed commit.

Resolve any PR conversations, then merge:

```bash
gh pr merge <PR_NUMBER> --repo scdqs/codex-app --merge --delete-branch
```

- [ ] **Step 6: Verify the merged files are public**

```bash
gh api repos/scdqs/codex-app/contents/SECURITY.md --jq '.html_url'
gh api repos/scdqs/codex-app/contents/CONTRIBUTING.md --jq '.html_url'
gh api repos/scdqs/codex-app/contents/.github/ISSUE_TEMPLATE/bug_report.yml --jq '.html_url'
gh api repos/scdqs/codex-app/contents/.github/pull_request_template.md --jq '.html_url'
```

Expected: every call returns a public GitHub URL on `main`.

### Task 4: Enable repository security controls

**Files:**
- No repository files.

**Interfaces:**
- Consumes: Merged `SECURITY.md` and the Security Advisories URL.
- Produces: Enabled private reporting, vulnerability alerts, and automated security-fix settings.

- [ ] **Step 1: Capture the current security state for rollback evidence**

```bash
gh api repos/scdqs/codex-app/private-vulnerability-reporting
gh api repos/scdqs/codex-app/vulnerability-alerts || true
gh api repos/scdqs/codex-app/automated-security-fixes
```

Expected pre-change state from design discovery: private reporting `false`, alerts disabled, automated fixes `false`.

- [ ] **Step 2: Enable Private vulnerability reporting**

```bash
gh api --method PUT repos/scdqs/codex-app/private-vulnerability-reporting
```

- [ ] **Step 3: Enable Dependabot alerts**

```bash
gh api --method PUT repos/scdqs/codex-app/vulnerability-alerts
```

- [ ] **Step 4: Enable automated security fixes**

```bash
gh api --method PUT repos/scdqs/codex-app/automated-security-fixes
```

This enables fix pull requests, not automatic merging.

- [ ] **Step 5: Verify all three controls**

```bash
gh api repos/scdqs/codex-app/private-vulnerability-reporting --jq '.enabled'
gh api repos/scdqs/codex-app/vulnerability-alerts --silent
gh api repos/scdqs/codex-app/automated-security-fixes --jq '{enabled,paused}'
```

Expected: private reporting prints `true`; the alerts endpoint exits 0; automated fixes prints `{"enabled":true,"paused":false}`.

Rollback, only if verification exposes an unintended setting:

```bash
gh api --method DELETE repos/scdqs/codex-app/private-vulnerability-reporting
gh api --method DELETE repos/scdqs/codex-app/vulnerability-alerts
gh api --method DELETE repos/scdqs/codex-app/automated-security-fixes
```

### Task 5: Protect `main` with an auditable repository ruleset

**Files:**
- Create temporarily: `/tmp/codex-main-ruleset.json`
- Do not commit the payload.

**Interfaces:**
- Consumes: Successful check context `release-gate`, GitHub Actions integration ID `15368`, maintainer user ID `5986625`, and merged governance PR.
- Produces: Active public repository ruleset `Protect main` targeting only `refs/heads/main`.

- [ ] **Step 1: Re-resolve immutable IDs and exact check context from current GitHub state**

```bash
gh api users/scdqs --jq '.id'
gh api "repos/scdqs/codex-app/commits/$(gh api repos/scdqs/codex-app/commits/main --jq '.sha')/check-runs" \
  --jq '.check_runs[] | select(.name == "release-gate") | {name,integration_id:.app.id,conclusion}'
gh api repos/scdqs/codex-app/rulesets --jq '.[] | {id,name,target,enforcement}'
gh api repos/scdqs/codex-app/branches/main/protection || true
```

Expected: user ID `5986625`; successful `release-gate` from integration `15368`; no pre-existing `Protect main` ruleset or classic branch protection. Stop instead of layering duplicate rules if another protection appeared.

- [ ] **Step 2: Create the exact ruleset payload in `/tmp/codex-main-ruleset.json`**

```json
{
  "name": "Protect main",
  "target": "branch",
  "enforcement": "active",
  "bypass_actors": [
    {
      "actor_id": 5986625,
      "actor_type": "User",
      "bypass_mode": "always"
    }
  ],
  "conditions": {
    "ref_name": {
      "include": ["refs/heads/main"],
      "exclude": []
    }
  },
  "rules": [
    {"type": "deletion"},
    {"type": "non_fast_forward"},
    {
      "type": "pull_request",
      "parameters": {
        "allowed_merge_methods": ["merge", "squash", "rebase"],
        "dismiss_stale_reviews_on_push": false,
        "require_code_owner_review": false,
        "require_last_push_approval": false,
        "required_approving_review_count": 0,
        "required_review_thread_resolution": true
      }
    },
    {
      "type": "required_status_checks",
      "parameters": {
        "do_not_enforce_on_create": false,
        "required_status_checks": [
          {
            "context": "release-gate",
            "integration_id": 15368
          }
        ],
        "strict_required_status_checks_policy": true
      }
    }
  ]
}
```

- [ ] **Step 3: Create the ruleset and retain its ID**

```bash
gh api --method POST repos/scdqs/codex-app/rulesets \
  --input /tmp/codex-main-ruleset.json \
  --jq '{id,name,target,enforcement,html_url}'
```

Expected: one active branch ruleset named `Protect main`. If GitHub rejects `User` bypass for a personal repository, do not create a weaker rule silently; report the API response and choose between a `RepositoryRole` administrator bypass or classic branch protection with the user.

- [ ] **Step 4: Verify the stored ruleset exactly**

```bash
gh api repos/scdqs/codex-app/rulesets/<RULESET_ID> \
  --jq '{name,target,enforcement,bypass_actors,conditions,rules}'

gh api repos/scdqs/codex-app/rules/branches/main \
  --jq '.[] | {type,ruleset_id,ruleset_source_type,ruleset_source}'
```

Confirm:

- Target is only `refs/heads/main`.
- Enforcement is active.
- Damon is the only always-bypass actor.
- Pull requests are required with zero approvals and resolved conversations.
- `release-gate` from integration `15368` is required and strict/up-to-date.
- Deletion and non-fast-forward pushes are blocked for non-bypass actors.
- No signed-commit rule exists.

- [ ] **Step 5: Verify the public Community Profile and final repository state**

```bash
gh api repos/scdqs/codex-app/community/profile \
  --jq '{health_percentage,files:{code_of_conduct:.files.code_of_conduct.url,contributing:.files.contributing.url,license:.files.license.url,readme:.files.readme.url,security:.files.security.url,issue_template:.files.issue_template.url,pull_request_template:.files.pull_request_template.url}}'

gh api repos/scdqs/codex-app/private-vulnerability-reporting --jq '.enabled'
gh api repos/scdqs/codex-app/vulnerability-alerts --silent
gh api repos/scdqs/codex-app/automated-security-fixes --jq '{enabled,paused}'
gh release view v0.1.22 --repo scdqs/codex-app --json url,isPrerelease,isDraft,tagName,targetCommitish
```

Expected: contribution, security, Issue, and PR template URLs are non-null; all security controls are enabled; the existing release remains an unchanged public pre-release.

- [ ] **Step 6: Test protection behavior with a disposable no-op branch only if needed**

The ruleset API result is the primary acceptance evidence. If UI/API ambiguity remains, open a disposable PR from a signed empty commit and confirm GitHub requires `release-gate`; do not attempt or force a destructive direct push to `main` merely to test rejection.

- [ ] **Step 7: Record recovery information and clean temporary files**

Record the ruleset ID and URL in the task handoff. Then remove only the known ephemeral payload and allowed-signers file:

```bash
rm /tmp/codex-main-ruleset.json
rm "$allowed_signers"
```

Ruleset rollback, only if its verified behavior differs from this plan:

```bash
gh api --method PUT repos/scdqs/codex-app/rulesets/<RULESET_ID> \
  -f enforcement=disabled
```

Do not delete the ruleset during normal rollback; disabling preserves an audit trail and is immediately reversible.
