# Codex Mobile Bridge

Codex Mobile Bridge 是一个 macOS 桌面桥接应用，让手机继续操作电脑上正在运行的 ChatGPT/Codex Desktop 任务。它不直接调用模型 API，也不替代桌面 Agent；电脑负责执行任务，手机负责查看会话、补充消息、创建会话和处理审批。

当前版本：`v0.1.5 Beta`

## 适用场景

- 使用 API 登录 ChatGPT/Codex Desktop，但无法使用官方手机 App。
- 离开电脑后，希望在手机上继续已有任务或处理临时审批。
- 需要局域网直连、临时公网链接，或使用自己的 Cloudflare 域名长期访问。

## 当前能力

- 自动检测并启动新版 `ChatGPT.app`，同时兼容旧版 `Codex.app`，排除 `ChatGPT Classic`。
- 通过 CDP 和 app-server RPC 读取真实桌面会话并回写消息。
- 手机 PWA 查看会话列表和完整消息流，最新消息保持在底部。
- 向现有会话发送文本和图片附件。
- 从手机创建新会话，并从 Bridge 提供的安全工作目录列表中选择工作空间。
- 查看并处理 Bridge 能捕获到的审批请求。
- 长期设备配对、自动恢复会话和桌面端撤销设备。
- 三种访问方式：局域网、Cloudflare 固定域名、Quick Tunnel 临时通道。
- 固定域名 Token 写入 macOS Keychain，诊断信息会脱敏。

## 当前限制

- 当前只实现 macOS，已验证的内部 DMG 为 Apple Silicon 架构。
- 电脑必须开机，ChatGPT/Codex Desktop 和 Bridge Service 必须保持运行。
- 手机端目前是 PWA，不是 App Store 或 Android 原生应用。
- Web Push、锁屏后台通知和多状态提示音仍属于后续功能，不能把设计文档当成已实现能力。
- 已配对手机在 MVP 中被视为可信本机用户，复杂权限分级尚未实现。
- 当前内部 DMG 使用 ad-hoc 签名，没有 Apple Developer ID 签名和 notarization，不属于 stable 公共发行包。
- CLI adapter、Windows 和 Linux 支持尚未实现。

## 工作原理

```text
Phone PWA
   |  authenticated HTTP / WebSocket
   v
Bridge Sidecar <--- Desktop Shell manages process, pairing and tunnels
   |
   |  CDP + app-server RPC
   v
ChatGPT.app / Codex.app
```

Bridge 不修改 ChatGPT/Codex 安装文件。桌面壳负责以 remote debugging port 启动或重新附着桌面应用，并管理 sidecar、配对链接和 Cloudflare 连接器。

## Mac App 快速体验

1. 安装并打开 `Codex Mobile Bridge.app`，确认窗口中显示的版本号与安装包一致。
2. 点击 ChatGPT/Codex 的检测或启动按钮。若桌面应用已经运行但没有开启 CDP，按提示允许 Bridge 重启它。
3. 点击 `Bridge Service / 启动`，等待状态变成 `ready`。只有连接状态为 `writable` 时手机才能发送消息。
4. 选择访问方式：
   - 同一 Wi-Fi：直接使用局域网地址。
   - 长期公网访问：配置 Cloudflare 固定域名。
   - 临时应急：手动启动 Quick Tunnel。
5. 在“手机配对”区域点击“生成新链接”，使用手机扫描二维码。

### 配对链接规则

- 带 `pairingToken=...` 的完整链接是一次性配对入口，使用后或过期后不能再次绑定新浏览器。
- 同一台手机、同一个浏览器成功配对后，可以反复打开当前 Bridge 根地址。
- 更换浏览器、清除站点数据、设备被撤销，或页面显示 `Unpaired`、`Needs new link`、`Session revoked or expired` 时，需要重新生成配对链接。
- 不要把包含 `pairingToken` 的链接转发给其他人。

## Cloudflare 固定域名

固定域名适合电脑长期在线、需要在外网重复访问的用户。它需要一个已托管在 Cloudflare 的域名，不需要路由器端口映射。

### 1. 创建 Named Tunnel

1. 登录 [Cloudflare Zero Trust](https://one.dash.cloudflare.com/)。
2. 进入 `Networks / 网络` -> `Connectors / 连接器` -> `Cloudflare Tunnels`。
3. 创建 `Cloudflared` 类型的 Tunnel，例如 `codex-mobile-bridge`。
4. 在安装 Connector 页面只复制 `--token` 后面的长 Tunnel Token。

不要运行 Cloudflare 给出的 `cloudflared service install ...` 完整命令。Bridge 会使用内置的 `cloudflared` 管理连接器生命周期。

### 2. 添加已发布应用程序路由

在 Tunnel 的“已发布应用程序路由”中添加：

| 字段 | 示例 |
| --- | --- |
| Public Hostname | `codex.example.com` |
| Path | 留空 |
| Service Type | `HTTP` |
| Service URL | `localhost:57324` |

不需要手动创建额外的 CNAME，也不要给这个子域名添加 Cloudflare Access 登录页、缓存或重写规则。

### 3. 在 Bridge 中连接

1. 打开 `远程访问`，选择 `固定域名`。
2. `Create Tunnel` 页面确认 Origin 为 `http://localhost:57324`，点击继续。
3. 在 `Connect Bridge` 填写：
   - `Public Hostname`：完整子域名，不带 `https://`。
   - `Tunnel Token`：只粘贴 Token，不粘贴终端命令。
   - `Local Port`：`57324`。
4. 保存后进入 `Verify`，点击“开始验证”。
5. 以下四项全部为 `ready` 才算配置完成：
   - `Local Bridge`
   - `Cloudflare connection`
   - `Public health`
   - `Same Bridge instance`

固定域名失败时，Bridge 只做有限自动重试，不会静默切换连接地址。用户可以修改配置、重新检测，或手动启动临时通道。

### 误装系统 Connector 的处理

如果曾运行 `cloudflared service install ...`，同一台 Mac 会同时出现系统 Connector 和 Bridge Connector。确认 Bridge 的 Verify 四项均为 `ready` 后，可执行：

```bash
sudo /opt/homebrew/bin/cloudflared service uninstall
```

这只删除重复的本机系统服务，不会删除 Cloudflare Tunnel、DNS 路由或 Bridge Keychain 中保存的 Token。

## Quick Tunnel 临时通道

- Quick Tunnel 会生成 `trycloudflare.com` 临时地址。
- 地址可能因进程退出、电脑睡眠、网络变化或 Cloudflare 回收而失效。
- Bridge 不会在固定域名失败时自动切换到 Quick Tunnel，必须由用户明确启动。
- Quick Tunnel 适合应急，不应作为稳定地址分发或收藏。

## 手机端行为

- 会话列表来自 ChatGPT/Codex Desktop 的真实 threads，不是独立云端数据库。
- 当前会话只请求有界事件窗口，并通过游标加载更早历史，避免大线程每次传输全部消息。
- HTTP 轮询结果是当前消息窗口的权威快照；仅保留尚未被服务端回显的本地 pending 消息，以避免重复显示。
- 新建会话必须选择 Bridge 返回且当前仍可用的工作目录，手机不能任意浏览整个 Mac 文件系统。
- 图片附件通过受认证的本地资源代理传递，诊断和事件响应不会暴露完整本机路径。

## 安全边界

- 一次性配对 Token 有有效期，并且成功使用后立即失效。
- 会话 API、图片资源和 WebSocket 都要求已配对设备的 session。
- Local Control API 不会挂载到手机公网路由。
- Tunnel Token 存在 macOS Keychain；启动 `cloudflared` 时通过权限受限的临时 token 文件传入，不出现在命令行参数和诊断中。
- 固定域名和 Quick Tunnel 互斥，关闭远程访问应真正停止由 Bridge 管理的 Connector。
- 当前没有账号体系或审批风险分级。请只配对可信设备，并在设备丢失时立即从桌面端撤销。
- 不要直接把 `57324` 端口映射到公网。

## 开发环境

需要 Rust、Node.js/npm、Xcode Command Line Tools，以及 Tauri 2 所需的 macOS 构建环境。

### 测试与检查

```bash
cargo test --workspace
cargo clippy -p desktop-shell -- -D warnings

cd apps/mobile-pwa
npm ci
npm test -- --run
npm run build

cd ../desktop-shell
npm ci
npm test -- --run
npm run build

cd ../..
./scripts/check-version-sync.sh
```

### 桌面开发

```bash
cargo build -p bridge-sidecar
(cd apps/mobile-pwa && npm ci && npm run build)

cd apps/desktop-shell
npm ci
npm run tauri:dev
```

开发模式会自动定位仓库根目录，并支持：

- `CODEX_MOBILE_BRIDGE_SIDECAR_BIN`：未设置时使用 `target/debug/bridge-sidecar`。
- `CODEX_MOBILE_BRIDGE_PWA_DIR`：未设置时使用 `apps/mobile-pwa/dist`。
- `CODEX_MOBILE_BRIDGE_ADVERTISED_HOST`：未设置时自动尝试 Wi-Fi/LAN IP。
- `CODEX_MOBILE_BRIDGE_DEBUG_PORT`：默认 `9229`。

### 构建 DMG

```bash
cd apps/desktop-shell
npm ci
npm run tauri:build -- --bundles dmg
```

构建过程会先编译 release sidecar 和 PWA，把 `bridge-sidecar` 与 `cloudflared` 放入 App resources，再执行 Tauri 打包。DMG 输出到：

```text
target/release/bundle/dmg/
```

没有正式 Apple 证书时，开发构建会自动使用 ad-hoc 签名。它能验证 App bundle 完整性，但不等于 Developer ID 签名或 Apple notarization。

## 诊断

本地和公网都可以检查：

```text
/api/health
```

正常响应应包含：

- `status: ok`
- `connectionState: writable`
- 当前 `version`
- 当前 `instanceId`

固定域名验证时，本地和公网的 `version`、`instanceId` 必须一致。公网返回旧版本通常表示旧 sidecar 仍占用固定端口；Cloudflare 显示正常但 Bridge Connector 已停止，通常表示机器上还残留独立系统 Connector。

常见桌面降级状态：

- `codex_not_running`：ChatGPT/Codex 尚未启动或没有完成有效诊断。
- `cdp_unavailable`：remote debugging port 不可达。
- `target_not_found`：CDP 可达，但没有识别到支持的桌面 page target。
- `inject_failed`：找到了页面，但 app-server bridge 注入失败。
- `rpc_unavailable`：注入成功，但基础 app-server RPC 不可用。
- `read_only`：可读取会话但不能回写。
- `writable`：可以读取会话并发送消息。

## 试用与发布

- 内部人工 QA：[docs/dogfood-qa-checklist.md](docs/dogfood-qa-checklist.md)
- 发布门禁：[docs/release-gates.md](docs/release-gates.md)
- 固定域名实现计划：[docs/superpowers/plans/2026-07-18-fixed-domain-named-tunnel.md](docs/superpowers/plans/2026-07-18-fixed-domain-named-tunnel.md)
- GitHub Actions 的 `Desktop build` workflow 可生成 dev/beta DMG。
- stable 版本必须具备 Developer ID 签名、Apple notarization 和 updater metadata。

## 参考

- [BigPizzaV3/CodexPlusPlus](https://github.com/BigPizzaV3/CodexPlusPlus)：CDP bridge 与 mobile relay 方向参考。
