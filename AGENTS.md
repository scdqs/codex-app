# Codex Mobile Bridge Project Guidance

## 产品目标

本项目让手机继续操作电脑上正在运行的 ChatGPT/Codex Desktop 任务。桌面 Agent 仍是唯一执行端；Bridge 负责读取会话、回写消息、处理审批、设备配对和远程访问，手机 PWA 不直接调用模型 API。

当前产品基线为 `v0.1.7 Beta`，优先支持 macOS Desktop App。CLI、Windows、Linux、原生手机 App 和复杂授权策略属于后续范围，除非用户明确提升优先级。

## 仓库结构

- `apps/desktop-shell/`：Tauri 桌面壳，负责 ChatGPT/Codex 启动、Bridge 生命周期、配对、设备管理和远程访问 UI。
- `apps/bridge-sidecar/`：本机 HTTP/WebSocket 服务入口，托管 PWA 并连接桌面 adapter。
- `apps/mobile-pwa/`：React 手机端，会话、消息、审批、新建会话、工作目录和图片附件 UI。
- `crates/bridge-core/`：CDP、app-server RPC、HTTP API、配对、存储、事件标准化与工作目录校验。
- `crates/desktop-core/`：sidecar 进程管理、ChatGPT/Codex 启动、Quick Tunnel、Named Tunnel、配置和诊断。
- `packages/bridge-protocol/`：前端共享协议与状态类型。
- `docs/`：设计、计划、QA 和发布门禁。计划文档不代表功能已经实现。

## 核心架构约束

### Desktop adapter

- 首选新版 `ChatGPT.app`，兼容旧版 `Codex.app`，必须排除 `ChatGPT Classic`。
- 不修改桌面应用安装文件；通过外部启动、CDP target 和 app-server RPC 接入。
- 不以“页面能打开”或单一 health 状态判断成功。可写链路至少要验证真实 `thread/list`、事件读取和 `turn/start`。
- CDP 大响应必须保持有界，避免通过扩大 frame limit 掩盖无界 payload。

### 会话同步

- `thread/list` 必须有界遍历 cursor 并按 thread ID 去重，不能假设第一页包含置顶或最近运行的全部会话。
- 事件 API 使用有界窗口和游标分页，不得每次返回完整大线程。
- app-server server notification 是实时增量主路径，HTTP polling 是断线恢复和权威校准；两者必须使用稳定事件 ID 合并。
- HTTP polling 返回的是当前可见窗口的权威快照，不得与旧 socket/local 事件无条件 append。
- 只允许尚未被服务端 user echo 覆盖的 `pending` optimistic message 暂时保留。
- 消息按旧到新展示，最新消息在底部；同 turn 同时间戳要保持稳定顺序。
- 不把 adapter 的大型 `raw` payload 暴露给手机端。
- reasoning summary、最终回答、plan 和工具状态必须分类型；不得把 `reasoning/textDelta` 或隐藏 chain-of-thought 暴露给手机。

### 项目层级

- 手机项目树按 canonical `cwd` 分组，名称使用可信 registry 或路径 basename。
- 折叠状态和手机置顶保存在手机本地；不得依赖 Desktop 私有 renderer store 才能正常工作。
- Desktop 私有置顶或项目顺序不可用时必须语义降级，不能导致会话缺失。

### 新建会话与附件

- 手机新建会话必须显式选择 Bridge 返回的工作目录。
- 服务端必须再次 canonicalize 并验证工作目录；不能只信任前端下拉选项，也不能开放任意文件系统浏览。
- 工作目录失效或离开允许范围时必须拒绝创建。
- 图片附件走受认证的本地资源代理，API、事件和诊断不得泄漏完整本机路径。

### 配对与安全

- Pairing Token 必须一次性、短时有效；设备 session 长期保存但可由桌面端撤销。
- 手机 REST、WebSocket 和图片资源必须鉴权。
- Local Control API 只能供本机桌面壳使用，绝不能挂载到公网 phone router。
- MVP 中已配对设备等同可信本机用户，可直接处理审批；不要擅自声称已实现风险分级。
- 诊断必须脱敏 Authorization、control/session/pairing token、Cloudflare Token、VAPID/API key 和完整本机路径。
- 诊断中的 PushSubscription 只允许 endpoint host、状态、最后成功时间和错误类别；不得包含 endpoint path/query、p256dh、auth 或 payload。

### 提醒与 Web Push

- 提醒事件固定为 completed、approval_required、input_required、error；检测、设置、payload 和去重均按类型建模。
- Web Push 只在 Named Tunnel 固定 HTTPS Origin 可用；Quick Tunnel、LAN 或 hostname 变化必须终止 pending/retry outbox，不能补发旧提醒。
- VAPID 私钥存入 macOS Keychain，只通过一次性 `0600` 文件交给 sidecar，读取后立即删除，不得放入命令行、日志或普通诊断。
- PushSubscription 绑定 authenticated device；替换、删除、404/410 失效和设备撤销必须清理相应状态/outbox。
- `(event_id, device_id)` 是 delivery 幂等键；网络错误总发送最多 4 次，单设备失败不得阻塞其他设备。
- Push payload 不得包含正文、CWD、工具参数、错误详情或其他大型/敏感字段。
- 页面可见时普通 push 只 postMessage；后台/锁屏显示系统通知；force test 即使页面可见也显示。
- WebSocket 与 Service Worker 必须共用前台 player 和 eventId 去重。iPhone 非 standalone 不得请求通知权限，permission denied 不得反复请求。

### 远程访问

- 局域网、Named Tunnel 固定域名和 Quick Tunnel 是明确模式，不得同时由 Bridge 启动多个 Connector。
- 固定域名默认使用固定端口 `57324`。端口占用时要明确报错，不得静默随机换端口。
- Named Tunnel Token 存入 macOS Keychain；启动 provider 时使用权限受限的临时 token 文件，不得放入命令行或日志。
- 不要求用户运行 `cloudflared service install`。Bridge 使用 bundle 内的 `cloudflared` 并负责其完整生命周期。
- 固定域名只做有限重试；配置或路由确认失败后停止，不得无限重连。
- 固定域名失败时不得自动切换 Quick Tunnel。只能提供由用户明确触发的临时通道入口。
- `关闭固定域名` 必须真正停止 App 管理的 Connector。
- 公网 ready 必须同时验证 local/public `version` 与 `instanceId` 一致，不能仅以 Cloudflare Tunnel 状态为准。

## 版本与发布

- `VERSION` 是产品版本源，桌面、sidecar、PWA、Cargo 和 Tauri manifest 必须同步。
- 任何进入 DMG 的用户可见行为修改都要提升 patch 版本并运行 `./scripts/check-version-sync.sh`。
- 纯文档、测试或不改变发行产物行为的修改不单独提升版本。
- dev/beta 无正式证书时允许 ad-hoc 签名，但不得称为已公证或 stable。
- stable 必须通过 `docs/release-gates.md`：Developer ID 签名、Apple notarization、updater metadata 和完整人工 QA。
- stable 启用 Web Push 时必须使用 Named Tunnel，并完成 iPhone 与 Android 真机 QA 确认。
- Bundle 必须包含 release `bridge-sidecar`、PWA dist 和 `cloudflared`；不要提交生成后的 resources、`target/` 或 `node_modules/`。

## 验证矩阵

根据改动范围运行最小充分集合。涉及共享协议、HTTP API、远程访问或发行包时运行完整集合：

```bash
cargo test --workspace
cargo clippy -p desktop-shell -- -D warnings

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

构建 DMG：

```bash
cd apps/desktop-shell
npm run tauri:build -- --bundles dmg
```

完成远程访问改动后，人工验证至少包括：

- Local Bridge、Cloudflare connection、Public health、Same Bridge instance 全部 ready。
- 移除任何独立系统 `cloudflared` 后固定域名仍可访问。
- 生成新配对链接，手机完成一次性配对并能再次打开根地址。
- 发送消息、图片、新建会话和审批回写至少各验证一次与改动相关的路径。
- 固定域名 Web Push 改动还要完成 `docs/qa/2026-07-18-web-push-device-matrix.md`；Quick Tunnel 必须验证不会请求 push 权限。

## 文档同步

- 新增或改变用户可见功能时，同一变更必须更新 `README.md`。
- 修改架构、安全不变量、测试命令或发布流程时，同一变更必须更新 `AGENTS.md`。
- 不把 `docs/plans/` 或 `docs/superpowers/` 中的规划能力写成已经交付。
- 示例必须使用 `example.com` 等占位域名，禁止提交真实 Tunnel Token、配对链接或用户固定域名。

## Skill routing

When the user's request matches an available skill, invoke that skill instead of recreating its workflow manually.

- Product ideas and problem framing: use `office-hours`.
- Strategy and scope review: use `plan-ceo-review`.
- Architecture and implementation planning: use `plan-eng-review`.
- Design planning and visual review: use `design-consultation`, `plan-design-review`, or `design-review` as appropriate.
- Full multi-discipline plan review: use `autoplan`.
- Bugs, errors, and regressions: use `investigate` or `diagnosing-bugs`.
- Browser QA and behavior verification: use `qa` or `qa-only`.
- Code or diff review: use `review` or `code-review`.
- Shipping, pull requests, and deployment: use `ship` or `land-and-deploy`.
- Save or restore long-running project context: use `context-save` or `context-restore`.

Follow the selected skill from its first required step, including its decision gates and verification requirements.
