# Mobile Session Experience and Alerts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复手机端会话缺失，交付项目层级、Codex 风格实时输出与思考摘要，并在同一实时状态管线上完成前台四状态提醒和固定域名 Web Push。

**Architecture:** Desktop adapter 对 `thread/list` 做有界全量分页；PWA 使用 `cwd` 构建本地项目树。Bridge 通过持久 CDP Runtime binding 订阅 ChatGPT/Codex app-server 通知，标准化后进入现有 EventHub 和认证 WebSocket，HTTP polling 仅承担权威快照与断线恢复。提醒检测复用 SessionSnapshot/实时状态流，Web Push 延续现有固定 HTTPS Origin、Keychain 和 sidecar 生命周期边界。

**Tech Stack:** Rust 2024、Tokio、CDP、Axum WebSocket、SQLite、React 19、TypeScript、Vite、Service Worker、Push API、IndexedDB、Vitest。

---

## What already exists

- `AppServerJsonRpcClient::list_threads` 已能映射单页 `thread/list`，需要补 cursor 循环，不新建第二套 adapter。
- `SessionSnapshot.cwd` 已跨 Rust/TypeScript 协议存在，项目树只需 PWA 分组和本地偏好。
- `EventHub`、认证 `/ws`、`SessionEvent` 和 HTTP 快照合并已存在，实时流复用这些边界。
- Desktop manager 已实测提供 `addNotificationCallback(methods, callback)`，无需抓 DOM 或解析渲染文本。
- 多状态提醒和 Web Push 已有详细计划：
  - `docs/superpowers/plans/2026-07-18-multi-state-alerts-foreground.md`
  - `docs/superpowers/plans/2026-07-18-direct-web-push.md`
- Named Tunnel、Keychain SecretStore、临时 token 文件和设备撤销已存在，Web Push 在这些机制上扩展。

## Execution order

```text
thread/list 全量分页
        │
        ▼
项目 → 会话层级
        │
        ▼
CDP 实时通知 → EventHub → 认证 WebSocket → PWA 增量 reducer
        │
        ├──────────────► 思考摘要 / 最终回答 / 工具状态
        │
        ▼
四状态 AlertDetector → 定向前台提醒
        │
        ▼
固定 HTTPS Origin → VAPID / PushSubscription / outbox → Service Worker
```

## Task 1: 有界全量 thread/list 分页

**Files:**
- Modify: `crates/bridge-core/src/codex_rpc.rs`
- Test: `crates/bridge-core/src/codex_rpc.rs`

- [x] **Step 1: 写多页、重复线程和重复 cursor 回归测试**

测试构造两页 `thread/list`，第一页返回 `nextCursor`，第二页包含目标旧会话；断言请求携带 `limit`、`sortKey: "recency_at"` 和 cursor。再构造重复 thread ID 与循环 cursor，断言去重且停止。

- [x] **Step 2: 运行失败测试**

Run: `cargo test -p bridge-core codex_rpc::tests::adapter_paginates_thread_list -- --exact`

Expected: FAIL，因为当前只请求第一页。

- [x] **Step 3: 实现有界分页**

在 adapter 层增加页大小、最大页数和最大线程数常量；每页调用：

```rust
json!({
    "limit": THREAD_LIST_PAGE_SIZE,
    "sortKey": "recency_at",
    "sortDirection": "desc",
    "cursor": cursor,
})
```

使用 `HashSet<String>` 去重 thread ID 和 cursor。达到上限、无 cursor、空 cursor或 cursor 重复时停止；响应结构无数组时返回 `InvalidResponse`。

- [x] **Step 4: 运行 adapter 与 HTTP 回归**

Run: `cargo test -p bridge-core codex_rpc`

Run: `cargo test -p bridge-core paired_device_can_list_live_codex_threads`

Expected: PASS。

## Task 2: 手机端项目层级与本地视图偏好

**Files:**
- Create: `apps/mobile-pwa/src/project-view.ts`
- Create: `apps/mobile-pwa/src/project-view.test.ts`
- Modify: `apps/mobile-pwa/src/storage.ts`
- Modify: `apps/mobile-pwa/src/App.tsx`
- Modify: `apps/mobile-pwa/src/App.test.tsx`
- Modify: `apps/mobile-pwa/src/styles.css`

- [x] **Step 1: 写项目分组和排序纯函数测试**

覆盖：相同规范化 cwd 分组、无 cwd 进入 Other sessions、本地置顶优先、项目按最近会话排序、项目内会话按置顶后更新时间排序。

- [x] **Step 2: 写本地偏好容错测试**

`storage.ts` 使用版本化 key 保存 `collapsedProjectIds` 与 `pinnedThreadIds`；无效 JSON、非字符串数组和旧数据返回空偏好。

- [x] **Step 3: 实现项目视图模型**

```ts
export interface SessionProjectGroup {
  id: string;
  label: string;
  cwd?: string;
  sessions: SessionSnapshot[];
  latestUpdatedAt: number;
}
```

项目 ID 使用规范化 cwd；label 使用路径 basename；不向服务端发送本地置顶或展开状态。

- [x] **Step 4: 替换 SessionList 为项目树**

每个项目提供展开/收起按钮与会话数量；会话行提供独立置顶按钮。新项目默认展开，用户主动收起后持久化。选择会话、关闭抽屉和审批标记保持现有行为。

- [x] **Step 5: 补移动端样式与可访问性**

项目标题使用 button、`aria-expanded`、键盘可操作；置顶按钮具有线程标题相关的 aria-label；窄屏不横向滚动。

- [x] **Step 6: 运行 PWA 测试与 build**

Run: `cd apps/mobile-pwa && npm test -- --run`

Run: `cd apps/mobile-pwa && npm run build`

Expected: PASS。

## Task 3: CDP app-server 实时通知流

**Files:**
- Modify: `crates/bridge-core/src/cdp.rs`
- Modify: `crates/bridge-core/src/codex_rpc.rs`
- Modify: `crates/bridge-core/src/normalizer.rs`
- Modify: `crates/bridge-core/src/protocol.rs`
- Modify: `crates/bridge-core/src/http_api.rs`
- Modify: `crates/bridge-core/src/event_hub.rs`
- Modify: `apps/bridge-sidecar/src/main.rs`
- Modify: `packages/bridge-protocol/src/protocol.ts`
- Modify: `apps/mobile-pwa/src/App.tsx`
- Modify: `apps/mobile-pwa/src/App.test.tsx`
- Modify: `apps/mobile-pwa/src/bridge-protocol.test.ts`
- Modify: `apps/mobile-pwa/src/styles.css`

- [x] **Step 1: 写 Desktop bridge 订阅脚本测试**

注入脚本必须暴露 `subscribeNotifications`，内部通过已验证的 manager `addNotificationCallback` 注册固定 method 集合；重新订阅先调用旧 unsubscribe，callback 只把 JSON 通知交给 CDP binding。

- [x] **Step 2: 写持久 CDP binding 测试**

测试 WebSocket 服务断言依次收到 `Runtime.enable`、`Runtime.addBinding`、`Runtime.evaluate`，随后发送 `Runtime.bindingCalled`；Rust stream 必须解析出 method/params，并忽略其他 CDP event。

- [x] **Step 3: 实现 CdpNotificationStream**

建立独立持久 DevTools WebSocket，安装固定 binding，调用 renderer `subscribeNotifications`，有界保存 setup 阶段提前到达的通知。连接断开返回错误，由 sidecar 有限退避重连。

- [x] **Step 4: 扩展类型化 SessionEvent**

增加：

```text
reasoning_summary
reasoning_summary_delta
plan
plan_delta
```

`item/agentMessage/delta` 只能映射 `message_delta`；`item/reasoning/summaryTextDelta` 只能映射 `reasoning_summary_delta`。手机 payload 不包含大型 raw，也不发送 `reasoning/textDelta` 隐藏推理正文。

- [x] **Step 5: 修复历史快照中的 reasoning/plan 规范化**

完成 turn item 的 `reasoning.summary[]` 映射为 `reasoning_summary`；plan 映射为 `plan`；最终 agentMessage 仍为 assistant message。HTTP 快照和 WebSocket 增量使用同一 item/turn ID，可在 polling 校准时替换。

- [x] **Step 6: Sidecar 启动实时 forwarder**

在 adapter 注入成功后启动 notification loop；每条通知通过 `AppState::apply_codex_notification` 记录事件、广播 envelope，并更新已有 snapshot 的 running/idle/error/waiting 状态。断线使用 250ms 到 5s 有限退避，不阻塞 HTTP 服务。

- [x] **Step 7: PWA 分类型增量 reducer 与 UI**

最终回答、思考摘要和 plan 分别按稳定 event ID 追加。运行中的思考摘要默认展开，turn 完成后可折叠；工具事件继续结构化显示。HTTP polling 仍作为权威快照，不保留已被快照覆盖的陈旧 live delta。

- [x] **Step 8: 运行 Rust、PWA 和 build 回归**

Run: `cargo test -p bridge-core cdp`

Run: `cargo test -p bridge-core normalizer`

Run: `cargo test -p bridge-core http_api`

Run: `cd apps/mobile-pwa && npm test -- --run && npm run build`

Expected: PASS。

## Task 4: 多状态前台提醒

**Source plan:** `docs/superpowers/plans/2026-07-18-multi-state-alerts-foreground.md`

- [x] **Step 1: 执行原计划 Task 1-3**

交付按设备定向 `alert_event`、通知设置存储、PublicAccessState 和 completed/approval_required/input_required/error 状态转换表。

- [x] **Step 2: 调整原计划 Task 4 的 monitor 数据源**

实时 `SessionSnapshot` 变化是主触发源；adapter 轮询保留为启动恢复和 5 秒兜底。两条路径共用持久 `session_alert_state` 和稳定 event ID，避免双提醒。

- [x] **Step 3: 执行原计划 Task 5-6**

交付四种固定 Web Audio tone、全局声音/震动开关、四类独立开关、试听、Settings 页面和首次引导。

- [x] **Step 4: 执行 Phase 2 自动回归，不单独发布 0.1.6**

因为本批次继续完成 Phase 3，版本直接在最终 Task 6 统一升级到 0.1.7；Phase 2 自动测试已执行，前台与真机人工 QA 统一保留到 Task 6 的设备矩阵。

## Task 5: 固定域名直接 Web Push

**Source plan:** `docs/superpowers/plans/2026-07-18-direct-web-push.md`

- [x] **Step 1: 执行原计划 Task 1-3**

交付 VAPID Keychain 生命周期、`0600` secret file、安全 sidecar 交接、PushSubscription、SQLite outbox、Web Push transport 和有限重试 worker。

- [x] **Step 2: 执行原计划 Task 4-5**

交付构建型 `/sw.js`、严格 payload parser、IndexedDB 原子 eventId 去重、系统通知、点击 deep-link、subscription enable/repair/disable 和 iPhone standalone 门槛。

- [x] **Step 3: 执行原计划 Task 6**

交付诊断脱敏、固定/临时模式能力显示、设备撤销清理、真机 QA 文档和稳定发布门禁。

## Task 6: 版本、文档与完整验证

**Files:**
- Modify: `VERSION`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `apps/mobile-pwa/package.json`
- Modify: `apps/mobile-pwa/package-lock.json`
- Modify: `apps/desktop-shell/package.json`
- Modify: `apps/desktop-shell/package-lock.json`
- Modify: `apps/desktop-shell/src-tauri/tauri.conf.json`
- Modify: `README.md`
- Modify: `AGENTS.md` only if architecture/test/release invariants change
- Modify: relevant dogfood/release docs

- [x] **Step 1: README 同步用户可见能力和边界**

写明项目层级、本地置顶、实时思考摘要、四状态前台提醒、固定域名锁屏 Web Push，以及 Quick Tunnel 不支持可靠锁屏提醒的边界。

- [x] **Step 2: 统一升级版本到 0.1.7**

Run: `./scripts/check-version-sync.sh`

Expected: PASS。

- [x] **Step 3: 完整自动验证**

```bash
cargo test --workspace
cargo clippy -p desktop-shell -- -D warnings
cd apps/mobile-pwa && npm test -- --run && npm run build
cd ../desktop-shell && npm test -- --run && npm run build
cd ../.. && ./scripts/check-version-sync.sh && git diff --check
```

- [x] **Step 4: 构建 Beta DMG**

```bash
cd apps/desktop-shell
npm run tauri:build -- --bundles dmg
```

已生成 Apple Silicon、ad-hoc 签名且未 notarize 的 `Codex Mobile Bridge_0.1.7_aarch64.dmg`；该产物只用于 Beta 内部测试，不是 stable 发行包。

- [ ] **Step 5: 完成人工与真机 QA**

人工验证：跨第二页旧会话出现；项目折叠/置顶重开后保留；最终回答和思考摘要实时追加；四状态前台声音不同；固定域名锁屏通知可达；Quick Tunnel 不请求 push 权限；设备撤销后 subscription/outbox 清理。Web Push 结果填写到 `docs/qa/2026-07-18-web-push-device-matrix.md`，在真实 iPhone 和 Android 完成前保持待测。

## Failure modes

| Flow | Production failure | Test/error handling/user result |
|---|---|---|
| thread pagination | cursor 循环或异常大历史 | cursor 去重和上限测试；返回已有有界结果，不无限请求 |
| project tree | localStorage 损坏 | parser 测试；回退空偏好，项目仍可用 |
| CDP stream | Desktop reload/DevTools socket close | stream 测试和 sidecar 退避重连；HTTP polling 继续工作 |
| reasoning stream | provider 不发送 summary | UI 只展示工具/状态，不伪造思考内容 |
| alert detector | poll 与 stream 同时观察相同转换 | 稳定 event ID + SQLite 状态测试；只提醒一次 |
| Web Push | 410/失效订阅 | 错误分类测试；订阅失效并提示重新启用 |
| Service Worker | 页面前台与 push 同时收到 | IndexedDB 原子 claim 测试；只展示/播放一次 |

## NOT in scope

- 原生 iOS/Android App：继续使用 PWA，避免引入第二套客户端。
- 跨端同步 Desktop 私有置顶/展开 store：第一版使用手机本地偏好，避免 Desktop 版本耦合。
- 模型隐藏的完整 chain-of-thought：只展示 provider/Desktop 提供的 reasoning summary 和执行过程。
- Quick Tunnel 锁屏 Web Push：Origin 不稳定，明确降级为前台提醒。
- Windows/Linux adapter：保持后续路线，不扩展本批 macOS Desktop MVP。

## Parallelization

顺序实现，不使用并行 worktree。分页、项目树、实时流和提醒都触及共享协议/PWA 状态；Web Push又依赖提醒设置和 dispatcher。并行会增加协议冲突和重复迁移风险。
