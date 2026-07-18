# Codex Mobile Bridge 发布门禁

本项目面向普通 Codex API 用户后，发布版本必须区分 dev、beta 和 stable。尤其是公网访问和 macOS 安装包不能把内部试用能力包装成稳定能力。

## 发布频道

| 频道 | 用途 | 签名/公证 | 更新元数据 | 远程访问 |
| --- | --- | --- | --- | --- |
| dev | 本机开发和代码审查 | 不要求 | 不要求 | 仅本机/局域网 |
| beta | 公司内部试点 | 可暂时 unsigned，但必须明确标注 | 可不启用 | Quick Tunnel 只允许显式 Beta |
| stable | 普通用户公开发布 | 必须 Developer ID 签名并 notarize | 必须有 updater metadata | 不允许把 Quick Tunnel 当默认稳定能力 |

## 稳定版硬性规则

- stable 不能发布 unsigned DMG。
- stable 不能缺少 notarization 配置。
- stable 不能缺少 updater 签名私钥和更新 manifest URL。
- stable 不能默认开启 Quick Tunnel。
- stable 如果还只有 Cloudflare Quick Tunnel，远程访问必须标注为 Beta/实验能力，不能写成“稳定远程连接”。
- stable 不应暴露 Local Control API 到公网 tunnel。
- stable 如果启用 Web Push，必须使用 Named Tunnel 固定域名，并完成 iPhone 与 Android 真机锁屏通知 QA。

## CI 门禁脚本

仓库提供 `scripts/check-release-gate.sh`：

```bash
scripts/check-release-gate.sh --channel dev
scripts/check-release-gate.sh --channel beta
scripts/check-release-gate.sh --channel stable
```

dev 默认通过，用来确保脚本和 workflow 本身可执行。

beta 如果设置 `ENABLE_QUICK_TUNNEL_BY_DEFAULT=true`，必须同时设置：

```bash
QUICK_TUNNEL_BETA_ACK=true
```

stable 需要以下环境变量之一组满足签名、公证和更新要求：

```bash
# 签名：二选一
APPLE_SIGNING_IDENTITY="Developer ID Application: ..."
MACOS_CERTIFICATE_P12="base64 encoded p12"
MACOS_CERTIFICATE_PASSWORD="..."

# 公证：API key 方式
APPLE_API_KEY_ID="..."
APPLE_API_ISSUER_ID="..."
APPLE_API_KEY="..."

# 或公证：Apple ID 方式
APPLE_ID="..."
APPLE_PASSWORD="..."
APPLE_TEAM_ID="..."

# 更新
TAURI_SIGNING_PRIVATE_KEY="..."
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="..."
UPDATE_MANIFEST_URL="https://..."
```

如果 stable 包含 Web Push，还必须显式设置：

```bash
ENABLE_WEB_PUSH=true
STABLE_REMOTE_ACCESS_PROVIDER=named_tunnel
PUSH_QA_IOS_ACK=true
PUSH_QA_ANDROID_ACK=true
```

这些确认表示已完成 `docs/qa/2026-07-18-web-push-device-matrix.md` 的真机测试；Quick Tunnel 和局域网模式不能作为 stable 锁屏通知通道。

如果传入 `RELEASE_ARTIFACT=/path/to/app.dmg`，脚本还会确认文件存在，并阻止 stable 使用文件名包含 `unsigned` 的 DMG。

## GitHub Actions

`.github/workflows/release-gates.yml` 在 push / PR 上默认跑 dev gate。手动触发 workflow 时可选择 `dev`、`beta` 或 `stable`。stable 会从 repository secrets 读取签名、公证和 updater 变量；缺失时 workflow 会失败，这是预期行为。

`.github/workflows/desktop-build.yml` 用于手动构建 macOS 试用包：

1. 在 Actions 里选择 **Desktop build**。
2. `channel` 选 `dev` 或 `beta` 生成内部试用包；`stable` 会先经过签名、公证和 updater metadata 门禁。
3. `bundles` 选 `dmg` 生成 DMG，或选 `app,dmg` 同时上传 `.app.zip` 和 DMG。
4. workflow 会运行 workspace 测试、准备 bundled sidecar/PWA resources、执行 Tauri build、收集 artifact，并对 artifact 再跑一次 release gate。

内部试用产物可以是 unsigned。stable 产物不能靠这个 workflow 绕过签名、公证和 updater metadata 检查。

## 进入 stable 前的人工检查

- [ ] 完成 `docs/dogfood-qa-checklist.md` 中的 install、pairing、send message、approval、revoke、diagnostics 流程。
- [ ] 使用一台没有开发环境的 macOS 机器验证安装包。
- [ ] 断网、换 Wi-Fi、手机锁屏后恢复，确认连接状态可理解。
- [ ] 诊断包人工检查无 token、API key、完整本机路径。
- [ ] README 和下载页不承诺当前实现做不到的远程稳定性。
