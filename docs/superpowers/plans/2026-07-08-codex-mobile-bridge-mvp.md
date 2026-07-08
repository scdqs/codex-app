# Codex Mobile Bridge MVP 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**目标：** 做出 macOS 优先的 Codex Mobile Bridge MVP：已配对手机 PWA 可以查看 Codex Desktop 会话流、接收实时状态、发送文本回复，并对可捕获的审批请求执行批准或拒绝。

**架构：** 本机 Rust sidecar 负责配对、设备认证、HTTP/WebSocket、SQLite 状态、CDP 连接、app-server JSON-RPC、事件归一化和审批路由。React/Vite PWA 只连接 sidecar 暴露的稳定协议，不直接接触 Codex、CDP、app-server 或 API Key。Codex Desktop 细节隔离在 `CodexAdapter` 边界后，后续 CLI、Windows/Linux、tunnel、云端中继都从 adapter 层扩展。

**技术栈：** Rust workspace、Tokio、Axum、Axum WebSocket、reqwest、rusqlite、serde、uuid、Vite、React、TypeScript、Vitest、Testing Library。

---

## 体量约束

用户已明确反馈长输出会触发 `stream disconnected before completion`。本计划采用压缩执行计划，不把完整实现代码提前塞进计划文件。执行阶段按任务逐段补测试和实现，每个任务结束后提交，确保计划可读、可审、可恢复。

## 范围确认

本计划覆盖设计规格中的第一至第三阶段，完成可内部使用的 MVP：

- macOS 本机 sidecar。
- 局域网二维码配对和长期设备绑定。
- 手机 PWA 会话列表、会话详情、实时事件和文本回复。
- Codex Desktop CDP/app-server 接入。
- 审批事件捕获、手机审批卡片和批准/拒绝回写。
- 设备撤销、断线恢复、诊断状态和降级原因。

不进入本计划的内容：

- 原生 iOS/Android App。
- 云端账号、多租户托管中继和公网推送。
- 中转 API 配置、模型 provider 管理和余额管理。
- Windows/Linux adapter 实现。
- 复杂授权策略、风险分级和 workspace 白名单。
- 完整远程控制，例如暂停、终止、重试、切换 workspace。

## 文件结构

```text
Cargo.toml
README.md
crates/bridge-core/Cargo.toml
crates/bridge-core/src/lib.rs
crates/bridge-core/src/protocol.rs
crates/bridge-core/src/storage.rs
crates/bridge-core/src/pairing.rs
crates/bridge-core/src/event_hub.rs
crates/bridge-core/src/http_api.rs
crates/bridge-core/src/cdp.rs
crates/bridge-core/src/codex_rpc.rs
crates/bridge-core/src/normalizer.rs
crates/bridge-core/src/approval.rs
crates/bridge-core/src/diagnostics.rs
apps/bridge-sidecar/Cargo.toml
apps/bridge-sidecar/src/main.rs
apps/mobile-pwa/package.json
apps/mobile-pwa/tsconfig.json
apps/mobile-pwa/vite.config.ts
apps/mobile-pwa/index.html
apps/mobile-pwa/src/main.tsx
apps/mobile-pwa/src/App.tsx
apps/mobile-pwa/src/api.ts
apps/mobile-pwa/src/protocol.ts
apps/mobile-pwa/src/storage.ts
apps/mobile-pwa/src/styles.css
apps/mobile-pwa/src/App.test.tsx
apps/mobile-pwa/src/api.test.ts
```

职责边界：

- `protocol.rs` / `protocol.ts`：sidecar 与 PWA 之间的稳定业务协议。
- `storage.rs`：SQLite schema、migration、设备、事件游标和会话缓存。
- `pairing.rs`：一次性 pairing token、长期设备绑定、session token、撤销。
- `event_hub.rs`：内存事件广播、WebSocket fan-out、快照 replay。
- `http_api.rs`：health、pairing、devices、sessions、commands、WebSocket 路由。
- `cdp.rs`：CDP target 发现、Codex page 选择、脚本注入和健康检查。
- `codex_rpc.rs`：app-server JSON-RPC client 与 `CodexAdapter` trait。
- `normalizer.rs`：Codex 原始事件到稳定业务事件的转换。
- `approval.rs`：审批请求识别、审批结果归一化、回写入口。
- `diagnostics.rs`：连接状态、降级原因、兼容性事实。
- `main.rs`：CLI/env 配置、服务启动、二维码 URL、静态 PWA 托管。

## 任务总览

1. 初始化 Rust workspace 与协议模型。
2. 建立 SQLite 存储与 migration。
3. 完成设备配对、长期绑定、撤销和 token 校验。
4. 完成事件中心和 WebSocket envelope。
5. 完成 sidecar HTTP API。
6. 初始化 React PWA 和协议镜像。
7. 完成 PWA 配对与连接状态。
8. 完成会话列表、会话详情、实时流和文本回复。
9. 完成 CDP target 发现与 bridge health。
10. 完成 app-server JSON-RPC adapter。
11. 完成事件归一化。
12. 完成审批捕获和手机审批决策。
13. 接入真实 Codex Desktop 路径、诊断和降级状态。
14. 补 README、烟测脚本和最终验收。

---

### Task 1: Rust Workspace 与协议模型

**文件：**
- Create: `Cargo.toml`
- Create: `crates/bridge-core/Cargo.toml`
- Create: `crates/bridge-core/src/lib.rs`
- Create: `crates/bridge-core/src/protocol.rs`

- [ ] 写测试 `approval_request_serializes_with_camel_case_fields`，验证 `threadId`、`riskHint`、`kind: "command"`。
- [ ] 写测试 `websocket_envelope_round_trips`，验证 `ServerEnvelope::SessionEvent` 序列化后可反序列化。
- [ ] 添加 workspace 依赖：`anyhow`、`async-trait`、`axum`、`futures-util`、`reqwest`、`rusqlite`、`serde`、`serde_json`、`sha2`、`thiserror`、`tokio`、`tower-http`、`uuid`。
- [ ] 定义 `SessionSnapshot`、`SessionStatus`、`SessionEvent`、`SessionEventType`、`ApprovalRequest`、`ApprovalDecision`、`ApprovalKind`、`DecisionKind`、`ServerEnvelope`、`ClientCommand`。
- [ ] 运行 `cargo test -p bridge-core protocol -- --nocapture`，期望通过。
- [ ] 提交：`git add Cargo.toml crates/bridge-core && git commit -m "feat: add bridge protocol types"`。

### Task 2: SQLite 存储

**文件：**
- Modify: `crates/bridge-core/src/lib.rs`
- Create: `crates/bridge-core/src/storage.rs`

- [ ] 写测试 `migrations_create_devices_and_events_tables`，使用 `tempfile` 创建临时数据库。
- [ ] 写测试 `revoked_device_is_not_returned_as_active`，覆盖设备撤销语义。
- [ ] 实现 `Storage::open(path)`、`Storage::migrate()`、`insert_device`、`revoke_device`、`active_devices`、`record_event_cursor`、`latest_event_cursor`。
- [ ] 表结构包括 `devices`、`event_cursors`、`session_snapshots`，时间统一存 Unix milliseconds。
- [ ] 运行 `cargo test -p bridge-core storage -- --nocapture`，期望通过。
- [ ] 提交：`git add crates/bridge-core && git commit -m "feat: add bridge storage"`。

### Task 3: 配对、设备绑定和撤销

**文件：**
- Modify: `crates/bridge-core/src/lib.rs`
- Create: `crates/bridge-core/src/pairing.rs`
- Modify: `crates/bridge-core/src/storage.rs`

- [ ] 写测试 `pairing_token_can_only_be_used_once`。
- [ ] 写测试 `expired_pairing_token_is_rejected`。
- [ ] 写测试 `revoked_device_cannot_create_session`。
- [ ] 实现 `PairingManager::create_token`、`register_device`、`create_session_token`、`validate_session_token`、`revoke_device`。
- [ ] pairing token 默认 5 分钟过期；session token 默认 24 小时过期，可通过构造参数覆盖。
- [ ] 错误类型固定为 `InvalidToken`、`ExpiredToken`、`TokenAlreadyUsed`、`DeviceRevoked`、`DeviceNotFound`。
- [ ] 运行 `cargo test -p bridge-core pairing -- --nocapture`，期望通过。
- [ ] 提交：`git add crates/bridge-core && git commit -m "feat: add device pairing"`。

### Task 4: 事件中心与 WebSocket envelope

**文件：**
- Modify: `crates/bridge-core/src/lib.rs`
- Create: `crates/bridge-core/src/event_hub.rs`

- [ ] 写测试 `subscriber_receives_published_session_event`。
- [ ] 写测试 `latest_snapshot_is_replayed_to_new_subscriber`。
- [ ] 实现 `EventHub::publish`、`subscribe`、`set_snapshot`、`snapshot_for_thread`、`all_snapshots`。
- [ ] 事件广播使用 `tokio::sync::broadcast`，快照缓存使用 `Arc<RwLock<HashMap<String, SessionSnapshot>>>`。
- [ ] 运行 `cargo test -p bridge-core event_hub -- --nocapture`，期望通过。
- [ ] 提交：`git add crates/bridge-core && git commit -m "feat: add bridge event hub"`。

### Task 5: Sidecar HTTP API

**文件：**
- Modify: `crates/bridge-core/src/lib.rs`
- Create: `crates/bridge-core/src/http_api.rs`
- Create: `apps/bridge-sidecar/Cargo.toml`
- Create: `apps/bridge-sidecar/src/main.rs`

- [ ] 写 API 测试 `health_returns_connection_state`。
- [ ] 写 API 测试 `unpaired_request_cannot_read_sessions`。
- [ ] 写 API 测试 `paired_device_can_list_snapshots`。
- [ ] 路由包括 `GET /api/health`、`POST /api/pairing/start`、`POST /api/pairing/complete`、`GET /api/devices`、`DELETE /api/devices/:id`、`GET /api/sessions`、`GET /api/sessions/:thread_id/events`、`POST /api/sessions/:thread_id/messages`、`POST /api/approvals/:approval_id/decision`、`GET /ws`。
- [ ] 所有会话数据接口读取 `Authorization: Bearer <session_token>` 并校验设备状态。
- [ ] `main.rs` 从 `CODEX_MOBILE_BRIDGE_BIND` 读取监听地址，默认 `0.0.0.0:57324`。
- [ ] 运行 `cargo test -p bridge-core http_api -- --nocapture` 和 `cargo check --workspace`，期望通过。
- [ ] 提交：`git add crates apps Cargo.toml && git commit -m "feat: add sidecar http api"`。

### Task 6: React PWA 基础工程

**文件：**
- Create: `apps/mobile-pwa/package.json`
- Create: `apps/mobile-pwa/tsconfig.json`
- Create: `apps/mobile-pwa/vite.config.ts`
- Create: `apps/mobile-pwa/index.html`
- Create: `apps/mobile-pwa/src/main.tsx`
- Create: `apps/mobile-pwa/src/App.tsx`
- Create: `apps/mobile-pwa/src/protocol.ts`
- Create: `apps/mobile-pwa/src/styles.css`

- [ ] 添加 Vite React、TypeScript、Vitest、Testing Library 依赖。
- [ ] 在 `protocol.ts` 镜像 Rust 协议类型，字段使用 camelCase。
- [ ] `App.tsx` 首屏就是移动工作台，不做 landing page。
- [ ] 布局包含连接状态栏、待处理队列、会话列表、会话详情、底部输入框。
- [ ] CSS 使用稳定高度和移动端安全区，避免底部输入框遮挡审批卡片。
- [ ] 运行 `cd apps/mobile-pwa && npm install && npm test -- --run && npm run build`，期望通过。
- [ ] 提交：`git add apps/mobile-pwa && git commit -m "feat: scaffold mobile pwa"`。

### Task 7: PWA 配对与连接状态

**文件：**
- Create: `apps/mobile-pwa/src/api.ts`
- Create: `apps/mobile-pwa/src/storage.ts`
- Modify: `apps/mobile-pwa/src/App.tsx`
- Modify: `apps/mobile-pwa/src/App.test.tsx`
- Create: `apps/mobile-pwa/src/api.test.ts`

- [ ] 写测试 `reads_pairing_payload_from_url`，覆盖二维码 URL 参数。
- [ ] 写测试 `stores_device_session_after_pairing`，覆盖本地保存。
- [ ] 写测试 `shows_revoked_or_expired_connection_error`。
- [ ] 实现 `completePairing`、`getHealth`、`connectWebSocket`、`saveSession`、`loadSession`、`clearSession`。
- [ ] UI 状态包括未配对、配对中、已连接、Codex 未启动、注入失败、只读降级、可回写。
- [ ] 运行 `cd apps/mobile-pwa && npm test -- --run`，期望通过。
- [ ] 提交：`git add apps/mobile-pwa && git commit -m "feat: add pwa pairing flow"`。

### Task 8: 会话列表、详情、实时流和文本回复

**文件：**
- Modify: `apps/mobile-pwa/src/api.ts`
- Modify: `apps/mobile-pwa/src/App.tsx`
- Modify: `apps/mobile-pwa/src/App.test.tsx`
- Modify: `apps/mobile-pwa/src/styles.css`

- [ ] 写测试 `renders_session_list_and_selects_thread`。
- [ ] 写测试 `merges_message_delta_into_current_assistant_message`。
- [ ] 写测试 `send_text_posts_to_selected_thread`。
- [ ] 实现 `listSessions`、`listSessionEvents`、`sendTextMessage`。
- [ ] WebSocket 收到 `message_delta` 时合并显示，避免一字一条 DOM 节点。
- [ ] 底部输入框禁用条件：未连接、只读降级、无选中 thread、发送中。
- [ ] 运行 `cd apps/mobile-pwa && npm test -- --run && npm run build`，期望通过。
- [ ] 提交：`git add apps/mobile-pwa && git commit -m "feat: add mobile session workbench"`。

### Task 9: CDP target 发现与 bridge health

**文件：**
- Modify: `crates/bridge-core/src/lib.rs`
- Create: `crates/bridge-core/src/cdp.rs`
- Modify: `apps/bridge-sidecar/src/main.rs`

- [ ] 写测试 `selects_codex_page_target_from_cdp_targets`。
- [ ] 写测试 `reports_missing_target_as_degraded`。
- [ ] 实现 `CdpClient::list_targets`、`select_codex_target`、`evaluate_on_target`、`bridge_health`。
- [ ] `select_codex_target` 优先匹配 Codex Desktop page target；无法确认时返回明确 `NoCodexTarget`。
- [ ] `main.rs` 读取 `CODEX_MOBILE_BRIDGE_DEBUG_PORT`，默认 `9229`，启动时打印诊断状态。
- [ ] 运行 `cargo test -p bridge-core cdp -- --nocapture`，期望通过。
- [ ] 提交：`git add crates apps && git commit -m "feat: add cdp bridge health"`。

### Task 10: app-server JSON-RPC Adapter

**文件：**
- Modify: `crates/bridge-core/src/lib.rs`
- Create: `crates/bridge-core/src/codex_rpc.rs`

- [ ] 写测试 `json_rpc_request_uses_incrementing_ids`。
- [ ] 写测试 `adapter_maps_thread_list_response`。
- [ ] 写测试 `adapter_sends_turn_start_for_user_text`。
- [ ] 定义 `CodexAdapter` trait：`list_threads`、`resume_thread`、`list_turns`、`send_user_message`、`subscribe_events`、`respond_approval`。
- [ ] 实现 `AppServerJsonRpcClient`，方法名覆盖 `thread/list`、`thread/resume`、`thread/turns/list`、`turn/start`。
- [ ] transport 通过 trait 隔离，便于 CDP 注入 transport 与测试 transport 共用同一 adapter。
- [ ] 运行 `cargo test -p bridge-core codex_rpc -- --nocapture`，期望通过。
- [ ] 提交：`git add crates/bridge-core && git commit -m "feat: add codex app-server adapter"`。

### Task 11: 事件归一化

**文件：**
- Modify: `crates/bridge-core/src/lib.rs`
- Create: `crates/bridge-core/src/normalizer.rs`

- [ ] 写测试 `normalizes_thread_list_to_session_snapshots`。
- [ ] 写测试 `normalizes_assistant_delta_to_message_delta_event`。
- [ ] 写测试 `normalizes_waiting_for_input_status`。
- [ ] 实现 `Normalizer::snapshot_from_thread`、`events_from_turns`、`event_from_raw_notification`。
- [ ] 状态映射固定为 `idle`、`running`、`waiting_for_input`、`waiting_for_approval`、`error`。
- [ ] 对未知 payload 保留 `raw` 字段并生成 `SessionEventType::Error` 或 `SessionEventType::StatusChanged`，不让 PWA 依赖 Codex 内部字段。
- [ ] 运行 `cargo test -p bridge-core normalizer -- --nocapture`，期望通过。
- [ ] 提交：`git add crates/bridge-core && git commit -m "feat: normalize codex events"`。

### Task 12: 审批捕获与手机决策

**文件：**
- Modify: `crates/bridge-core/src/lib.rs`
- Create: `crates/bridge-core/src/approval.rs`
- Modify: `crates/bridge-core/src/http_api.rs`
- Modify: `apps/mobile-pwa/src/App.tsx`
- Modify: `apps/mobile-pwa/src/api.ts`
- Modify: `apps/mobile-pwa/src/App.test.tsx`

- [ ] 写 Rust 测试 `detects_command_approval_request_from_raw_payload`。
- [ ] 写 Rust 测试 `approval_decision_routes_to_adapter`。
- [ ] 写 PWA 测试 `renders_approval_card_and_approve_reject_buttons`。
- [ ] 实现 `ApprovalDetector::detect`，输出 `ApprovalRequest`，kind 覆盖 `command`、`file_edit`、`network`、`mcp`、`unknown`。
- [ ] `POST /api/approvals/:approval_id/decision` 写入 `deviceId`、`decision`、`decidedAt` 并调用 `CodexAdapter::respond_approval`。
- [ ] PWA 待处理队列优先展示审批卡片，按钮点击后立即进入处理中状态，返回结果进入会话流。
- [ ] 运行 `cargo test -p bridge-core approval -- --nocapture` 和 `cd apps/mobile-pwa && npm test -- --run`，期望通过。
- [ ] 提交：`git add crates apps && git commit -m "feat: add mobile approval flow"`。

### Task 13: 真实 Codex Desktop 接入、诊断和降级

**文件：**
- Modify: `crates/bridge-core/src/cdp.rs`
- Modify: `crates/bridge-core/src/codex_rpc.rs`
- Modify: `crates/bridge-core/src/diagnostics.rs`
- Modify: `crates/bridge-core/src/http_api.rs`
- Modify: `apps/bridge-sidecar/src/main.rs`

- [ ] 写测试 `diagnostics_reports_writable_when_rpc_methods_pass_health_check`。
- [ ] 写测试 `diagnostics_reports_read_only_when_turn_start_is_unavailable`。
- [ ] 实现注入 transport：通过 CDP evaluate 调用 Codex page 内的 app-server client。
- [ ] health check 顺序：CDP target 可达、bridge 脚本已注入、`thread/list` 可用、`thread/turns/list` 可用、`turn/start` 可用、审批回写能力可探测。
- [ ] 降级状态固定为 `codex_not_running`、`cdp_unavailable`、`target_not_found`、`inject_failed`、`rpc_unavailable`、`read_only`、`writable`。
- [ ] sidecar 启动后把 diagnostics 推送到 PWA 连接状态栏。
- [ ] 运行 `cargo test -p bridge-core diagnostics cdp codex_rpc -- --nocapture` 和 `cargo check --workspace`，期望通过。
- [ ] 提交：`git add crates apps && git commit -m "feat: wire codex desktop adapter"`。

### Task 14: README、烟测和最终验收

**文件：**
- Create: `README.md`
- Modify: `apps/bridge-sidecar/src/main.rs`
- Modify: `apps/mobile-pwa/package.json`

- [ ] README 写清 MVP 范围、开发命令、Codex Desktop 启动要求、局域网安全边界、常见降级原因。
- [ ] 添加 `npm run test:run`，命令为 `vitest run`。
- [ ] sidecar 启动输出 PWA URL 和二维码文本 URL：`http://<lan-ip>:57324/?pairingToken=...`。
- [ ] 手动烟测 1：启动 Codex Desktop remote debugging port，运行 `cargo run -p bridge-sidecar`，确认 `/api/health` 返回 `writable` 或明确降级原因。
- [ ] 手动烟测 2：手机或同网浏览器打开 PWA URL，完成配对，刷新后保持登录。
- [ ] 手动烟测 3：选择 thread，发送文本，Codex Desktop 对应 thread 继续执行。
- [ ] 手动烟测 4：触发需要确认的命令或文件编辑，PWA 出现审批卡片，批准或拒绝能让桌面任务继续。
- [ ] 最终自动验证：`cargo test --workspace -- --nocapture`、`cargo check --workspace`、`cd apps/mobile-pwa && npm test -- --run && npm run build`。
- [ ] 提交：`git add README.md apps crates && git commit -m "docs: add bridge runbook"`。

---

## 自检清单

规格覆盖：

- 会话列表、完整会话流、实时增量输出：Task 4、5、8、11。
- 文本回复回写 Codex Desktop：Task 8、10、13、14。
- 审批展示与批准/拒绝：Task 12、13、14。
- 长期设备绑定和撤销：Task 2、3、5、7。
- macOS Desktop App 优先：Task 9、10、13。
- 局域网 PWA：Task 5、6、7、14。
- 降级状态和诊断：Task 9、13、14。
- CLI、Windows/Linux、云端中继、原生 App：明确排除在 MVP 外，并由 adapter 边界保留扩展点。

占位符检查：

- 本计划不使用空占位、延期实现口号或“细节以后补”的写法。
- 每个任务都有明确文件、测试名、实现边界、验证命令和提交命令。
- 代码体量留到执行阶段按任务生成，避免再次制造 1500 行以上的计划文件。

类型一致性：

- Rust 与 TypeScript 协议统一使用 camelCase JSON。
- `SessionSnapshot`、`SessionEvent`、`ApprovalRequest`、`ApprovalDecision` 是跨端核心类型。
- `CodexAdapter` 是唯一回写 Codex 的业务接口。
- `Diagnostics` 是 PWA 展示连接/降级状态的唯一来源。

## 执行交接

Plan complete and saved to `docs/superpowers/plans/2026-07-08-codex-mobile-bridge-mvp.md`.

Two execution options:

1. **Subagent-Driven（推荐）**：每个任务派一个新 subagent 执行，我在任务间 review，适合这个多模块项目。
2. **Inline Execution**：当前会话直接按任务批量执行，每个阶段做 checkpoint，适合你想减少上下文切换时使用。
