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
