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
