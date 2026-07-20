# Codex Mobile Bridge

Codex Mobile Bridge 是一个 macOS 桌面桥接应用，让手机继续操作电脑上正在运行的 ChatGPT/Codex Desktop 任务。它不直接调用模型 API，也不替代桌面 Agent；电脑负责执行任务，手机负责查看会话、补充消息、创建会话和处理审批。

当前版本：`v0.1.18 Beta`

## 适用场景

- 使用 API 登录 ChatGPT/Codex Desktop，但无法使用官方手机 App。
- 离开电脑后，希望在手机上继续已有任务或处理临时审批。
- 需要局域网直连、临时公网链接，或使用自己的 Cloudflare 域名长期访问。

## 当前能力

- 自动检测并启动新版 `ChatGPT.app`，同时兼容旧版 `Codex.app`，排除 `ChatGPT Classic`。
- 桌面应用使用符合现代 macOS 规范的透明连续圆角图标；PWA、主屏幕快捷方式和系统通知使用对应的普通与 Maskable 图标。
- 通过 CDP 和 app-server RPC 读取真实桌面会话并回写消息。
- 手机 PWA 查看会话列表和完整消息流，最新消息保持在底部；首次扫码或重新打开时优先进入最近的主会话，不会被更新时间更晚的内部子任务抢占；缺少 Desktop 标题的子任务仍会用 Agent 路径和昵称生成可读标题。
- 按规范化工作目录展示“项目 → 会话”两层结构；项目折叠和会话置顶保存在当前手机。
- 手机和 Web 宽屏都可通过顶部菜单打开统一会话抽屉，并从抽屉进入提醒设置。
- 手机顶栏采用双层信息布局：主操作、产品名和连接状态位于第一层，Bridge 版本以小字显示在产品名下方，第二层状态提示可点击并通过底部抽屉完整展示；底部输入区减少留白且不再显示会话标题占位文案。
- 通过 app-server 实时通知流增量展示最终回答、思考摘要、计划和执行进度；搜索、读取文件、列出目录、运行命令/测试/构建、修改文件、Web/MCP 工具、图片和子任务等过程会显示语义化的运行与完成状态，同一 turn 聚合在一个 Codex 回复容器内，HTTP 游标同步负责断线恢复。
- 实时工具过程使用稳定 item ID 与轮询快照合并；即使 Desktop 的 turn 快照省略工具 item，Bridge 也会保留已收到的权威执行过程，同时只向手机暴露文件名或目录名等有界信息，不发送完整本机路径和大型原始 payload。
- 手机发送的 optimistic 消息、Bridge 回显和 Desktop 正式消息会按来源协调；运行中的 HTTP turn 快照会替换对应的实时临时事件，避免用户消息、回答片段和空 Thinking 重复；内部状态迁移不再显示成原始消息卡。
- 上下文自动压缩属于正常执行进度，不会在手机端误显示 Error 或触发错误提醒。
- 向现有会话发送文本和图片附件。
- 从手机创建新会话，并从 Bridge 提供的安全工作目录列表中选择工作空间。
- 查看并处理 Bridge 能捕获到的审批请求；长审批内容默认折叠为三行，展开后在有界区域内滚动，拒绝和允许操作始终可见。
- 长期设备配对、自动恢复会话和桌面端撤销设备。
- 三种访问方式：局域网、Cloudflare 固定域名、Quick Tunnel 临时通道。
- 四类任务提醒：完成、等待审批、等待输入和错误；前台提供不同提示音与可用时的震动。
- Cloudflare 固定 HTTPS 域名下支持直接 Web Push、锁屏系统通知、订阅修复和通知点击回到对应会话。
- 固定域名 Token 写入 macOS Keychain，诊断信息会脱敏。

## 当前限制

- 当前只实现 macOS，已验证的内部 DMG 为 Apple Silicon 架构。
- 电脑必须开机，ChatGPT/Codex Desktop 和 Bridge Service 必须保持运行。
- 手机端目前是 PWA，不是 App Store 或 Android 原生应用。
- 手机只展示 ChatGPT/Codex Desktop 或 provider 已提供的思考摘要和结构化执行过程，不暴露或伪造模型隐藏的完整 chain-of-thought。
- Quick Tunnel 和局域网只支持页面前台提醒；可靠锁屏通知需要固定 HTTPS 域名。
- iPhone 必须把 PWA 添加到主屏幕并从主屏幕打开后，才能请求系统通知权限；锁屏声音和震动最终由 iOS 控制。
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
- Bridge 会有界遍历 `thread/list` 分页，因此较早创建但最近仍在运行或置顶的会话不会因只读取第一页而缺失。
- 会话按 canonical `cwd` 分组为项目；手机端折叠和置顶是本地偏好，不依赖 Desktop 私有 UI store。
- 当前会话只请求有界事件窗口，并通过游标加载更早历史，避免大线程每次传输全部消息。
- app-server 实时通知经认证 WebSocket 增量到达；HTTP 游标结果仍是恢复和校准依据，仅保留尚未被服务端回显的本地 pending 消息。
- 思考区域只展示 Desktop 可展示的 reasoning summary；最终回答、计划和工具状态使用独立事件类型，避免混在一起。
- 顶部连接信息分两层展示，版本号位于产品名下方；第二层状态提示过长时保持单行省略，点击后由底部抽屉展示完整内容。长审批内容默认折叠，展开后只滚动内容区域，操作按钮不会被推出可视范围；消息输入框保持空白视觉占位并压缩底部留白。
- 新建会话必须选择 Bridge 返回且当前仍可用的工作目录，手机不能任意浏览整个 Mac 文件系统。
- 图片附件通过受认证的本地资源代理传递，诊断和事件响应不会暴露完整本机路径。

## 手机提醒

- 在 Settings 中可以分别开关完成、等待审批、等待输入和错误提醒，并试听四种前台提示音。
- 固定 HTTPS 模式可启用系统通知；状态会显示 `Active`、`Not enabled`、`Blocked`、`Needs repair` 或 `Unavailable`。
- `Repair notifications` 会清理浏览器与 Bridge 的旧订阅后重新注册；`Disable alerts` 会先关闭服务端总开关，再尝试清理两侧订阅。
- 页面可见时普通 push 只转发到页面，并与 WebSocket 使用同一个 `eventId` 去重；后台或锁屏时由 Service Worker 显示系统通知。
- 点击系统通知会聚焦或打开 PWA，并在会话列表加载后选择对应 thread；会话已不存在时会显示明确提示。
- Quick Tunnel 地址不稳定，不会请求或复用 PushSubscription，也不会承诺锁屏通知。

## 安全边界

- 一次性配对 Token 有有效期，并且成功使用后立即失效。
- 会话 API、图片资源和 WebSocket 都要求已配对设备的 session。
- Local Control API 不会挂载到手机公网路由。
- Tunnel Token 存在 macOS Keychain；启动 `cloudflared` 时通过权限受限的临时 token 文件传入，不出现在命令行参数和诊断中。
- VAPID 私钥存在 macOS Keychain，只通过一次性 `0600` 文件交给 sidecar 并在读取后删除；PushSubscription 与 outbox 绑定已配对设备。
- Web Push payload 只含事件类别、thread ID/标题和时间，不含消息正文、CWD、工具参数或错误详情。
- 固定域名和 Quick Tunnel 互斥，关闭远程访问应真正停止由 Bridge 管理的 Connector。
- 当前没有账号体系或审批风险分级。请只配对可信设备，并在设备丢失时立即从桌面端撤销。
- 不要直接把 `57324` 端口映射到公网。

## 开发环境

需要 Rust、Node.js/npm、Xcode Command Line Tools，以及 Tauri 2 所需的 macOS 构建环境。

### 构建缓存控制

项目构建入口根据 Git common-dir 定位主工作树，并让所有 Git worktree 共用其同级的 `codex-app-shared-target/`，避免每个任务副本重复保存 Rust/Tauri 编译产物。开发和测试 profile 关闭 incremental 以控制磁盘增长。

项目提供带缓存检查的 Cargo 入口；构建前后如果共享目录达到 20 GB，会在终端警告并发送一次 macOS 通知，不启动后台服务，也不会自动删除文件：

```bash
./scripts/cargo.sh test --workspace
./scripts/check-build-cache.sh
./scripts/cargo.sh clean
```

缓存降回阈值以下后，下一次超过阈值时会再次通知。优先使用项目脚本，确保位于任意路径的 worktree 都解析到同一共享目录；直接运行原始 `cargo` 命令不会触发预警，位于其他父目录的 worktree 还可能落入各自的静态 fallback target。

### 测试与检查

```bash
./scripts/cargo.sh test --workspace
./scripts/cargo.sh clippy -p desktop-shell -- -D warnings

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
./scripts/cargo.sh build -p bridge-sidecar
(cd apps/mobile-pwa && npm ci && npm run build)

cd apps/desktop-shell
npm ci
npm run tauri:dev
```

开发模式会自动定位仓库根目录，并支持：

- `CODEX_MOBILE_BRIDGE_SIDECAR_BIN`：未设置时使用共享 target 中的 `debug/bridge-sidecar`。
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
<scripts/cargo-target-dir.sh 输出>/release/bundle/dmg/
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
- Web Push 真机矩阵：[docs/qa/2026-07-18-web-push-device-matrix.md](docs/qa/2026-07-18-web-push-device-matrix.md)
- 固定域名实现计划：[docs/superpowers/plans/2026-07-18-fixed-domain-named-tunnel.md](docs/superpowers/plans/2026-07-18-fixed-domain-named-tunnel.md)
- GitHub Actions 的 `Desktop build` workflow 可生成 dev/beta DMG。
- stable 版本必须具备 Developer ID 签名、Apple notarization 和 updater metadata。

## 参考

- [BigPizzaV3/CodexPlusPlus](https://github.com/BigPizzaV3/CodexPlusPlus)：CDP bridge 与 mobile relay 方向参考。
