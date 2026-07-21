# 多状态提醒与前台提示 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 Mac Bridge 后台监控全部 Codex 会话，为 completed、approval required、input required、error 四类事件生成稳定且不重复的提醒，并在手机页面前台按设备设置播放不同声音和震动。

**Architecture:** Bridge 新增纯 `AlertDetector`、SQLite `NotificationStore`、自适应 `AlertMonitor` 和按设备定向的 `NotificationDispatcher`。PWA 把通知设置、能力检测、前台声音和首次引导拆出 `App.tsx`；本阶段只提供前台提醒，Web Push 与锁屏系统通知在 Phase 3 接入同一 `AlertEvent.eventId`。

**Tech Stack:** Rust 2024、Tokio、Rusqlite、Axum WebSocket、React 19、TypeScript、Web Audio API、Vibration API、Vitest、Testing Library。

---

## 文件结构

- Create `crates/bridge-core/src/alert_detector.rs`: 纯状态机、稳定 event ID、乱序保护和四类状态转换。
- Create `crates/bridge-core/src/notification_store.rs`: 每设备设置和每会话提醒状态的 SQLite 持久化。
- Create `crates/bridge-core/src/alert_monitor.rs`: adapter 轮询、approval 合并、自适应频率和失败退避。
- Create `crates/bridge-core/src/notification_dispatcher.rs`: 按设备设置过滤并发送定向 WebSocket alert。
- Create `crates/bridge-core/src/public_access.rs`: Bridge 内存中的 local/quick/named 能力状态。
- Modify `crates/bridge-core/src/protocol.rs`, `packages/bridge-protocol/src/protocol.ts`: 共享 `AlertKind`、`AlertEvent` 和 `alert_event` envelope。
- Modify `crates/bridge-core/src/event_hub.rs`: 增加按 device ID 定向广播，普通 session 事件仍全局广播。
- Modify `crates/bridge-core/src/http_api.rs`: Settings、测试提醒、remote access control API 和 WebSocket device context。
- Modify `apps/bridge-sidecar/src/main.rs`: 打开 NotificationStore 并启动 AlertMonitor。
- Modify `apps/desktop-shell/src-tauri/src/main.rs`: Named/Quick/local 切换时同步 Bridge 的 public access context。
- Create `apps/mobile-pwa/src/notifications/api.ts`: Settings API client 和严格 response parsing。
- Create `apps/mobile-pwa/src/notifications/capabilities.ts`: 前台声音、震动、固定 Origin 能力检测。
- Create `apps/mobile-pwa/src/notifications/foreground-alert-player.ts`: eventId 去重、四种 Web Audio 音色和震动模式。
- Create `apps/mobile-pwa/src/notifications/NotificationSettingsPage.tsx`: 完整 Settings 页面。
- Create `apps/mobile-pwa/src/notifications/NotificationOnboardingSheet.tsx`: 首次配对后一次性启用引导。
- Modify `apps/mobile-pwa/src/App.tsx`, `apps/mobile-pwa/src/styles.css`: Settings 导航、WS alert 处理和前台提示。
- Modify `VERSION` 及版本清单：完成 Phase 2 后统一升至 `0.1.6`。

## Task 1: 共享 Alert 协议与按设备 WebSocket 广播

**Files:**
- Modify: `crates/bridge-core/src/protocol.rs:45-53,124-150,152-210`
- Modify: `packages/bridge-protocol/src/protocol.ts:9-17,115-159,194-270`
- Modify: `apps/mobile-pwa/src/bridge-protocol.test.ts`
- Modify: `crates/bridge-core/src/event_hub.rs:1-109,111-185`
- Modify: `crates/bridge-core/src/http_api.rs:1490-1534`
- Test: `crates/bridge-core/src/protocol.rs`
- Test: `crates/bridge-core/src/event_hub.rs`

- [ ] **Step 1: 写 Rust alert envelope 序列化测试**

```rust
#[test]
fn alert_event_envelope_serializes_for_mobile_protocol() {
    let envelope = ServerEnvelope::AlertEvent(AlertEvent {
        event_id: "alert-abc".to_string(),
        kind: AlertKind::ApprovalRequired,
        thread_id: "thread-1".to_string(),
        thread_title: "Release v0.1.6".to_string(),
        occurred_at: 1_784_349_000_000,
    });

    assert_eq!(
        serde_json::to_value(envelope).unwrap(),
        json!({
            "type": "alert_event",
            "payload": {
                "eventId": "alert-abc",
                "kind": "approval_required",
                "threadId": "thread-1",
                "threadTitle": "Release v0.1.6",
                "occurredAt": 1_784_349_000_000u64
            }
        })
    );
}
```

- [ ] **Step 2: 运行协议测试确认失败**

Run:

```bash
cargo test -p bridge-core alert_event_envelope_serializes_for_mobile_protocol -- --exact
```

Expected: FAIL，因为 `AlertKind`、`AlertEvent` 和 envelope variant 尚不存在。

- [ ] **Step 3: 实现 Rust 协议类型**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    Completed,
    ApprovalRequired,
    InputRequired,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertEvent {
    pub event_id: String,
    pub kind: AlertKind,
    pub thread_id: String,
    pub thread_title: String,
    pub occurred_at: u64,
}
```

在 `ServerEnvelope` 增加：

```rust
AlertEvent(AlertEvent),
```

- [ ] **Step 4: 写 TypeScript parser 失败测试**

在 `bridge-protocol.test.ts` 增加：

```ts
it("accepts all four Rust-compatible alert event kinds", () => {
  for (const kind of ["completed", "approval_required", "input_required", "error"] as const) {
    expect(isServerEnvelope({
      type: "alert_event",
      payload: {
        eventId: `event-${kind}`,
        kind,
        threadId: "thread-1",
        threadTitle: "Task",
        occurredAt: 1784349000000,
      },
    })).toBe(true);
  }
});
```

- [ ] **Step 5: 实现 TypeScript 对等类型**

```ts
export const ALERT_KINDS = [
  "completed",
  "approval_required",
  "input_required",
  "error",
] as const;

export type AlertKind = (typeof ALERT_KINDS)[number];

export interface AlertEvent {
  eventId: string;
  kind: AlertKind;
  threadId: string;
  threadTitle: string;
  occurredAt: number;
}
```

`ServerEnvelope` 增加：

```ts
| { type: "alert_event"; payload: AlertEvent }
```

`isServerEnvelope` 和新增 `isAlertEvent` 必须严格验证所有字段及 enum。

- [ ] **Step 6: 写定向广播隔离测试**

```rust
#[tokio::test]
async fn targeted_alert_is_only_received_by_the_matching_device() {
    let hub = EventHub::new();
    let mut phone_a = hub.subscribe_for_device("phone-a").await;
    let mut phone_b = hub.subscribe_for_device("phone-b").await;
    let alert = ServerEnvelope::AlertEvent(alert("alert-1"));

    hub.publish_to_device("phone-a", alert.clone());

    assert_eq!(phone_a.recv().await.unwrap(), alert);
    assert!(tokio::time::timeout(Duration::from_millis(25), phone_b.recv()).await.is_err());
}

#[tokio::test]
async fn global_session_events_still_reach_every_device() {
    let hub = EventHub::new();
    let mut phone_a = hub.subscribe_for_device("phone-a").await;
    let mut phone_b = hub.subscribe_for_device("phone-b").await;
    let envelope = ServerEnvelope::SessionEvent(session_event());

    hub.publish(envelope.clone());

    assert_eq!(phone_a.recv().await.unwrap(), envelope);
    assert_eq!(phone_b.recv().await.unwrap(), envelope);
}

#[tokio::test]
async fn disconnect_signal_closes_only_the_revoked_device_subscriber() {
    let hub = EventHub::new();
    let mut phone_a = hub.subscribe_for_device("phone-a").await;
    let mut phone_b = hub.subscribe_for_device("phone-b").await;

    hub.disconnect_device("phone-a");

    assert!(matches!(phone_a.recv().await, Err(EventReceiveError::DeviceDisconnected)));
    hub.publish(ServerEnvelope::SessionEvent(session_event()));
    assert!(matches!(phone_b.recv().await, Ok(ServerEnvelope::SessionEvent(_))));
}
```

- [ ] **Step 7: 运行 EventHub 测试确认失败**

Run:

```bash
cargo test -p bridge-core targeted_alert_is_only_received_by_the_matching_device -- --exact
```

Expected: FAIL，因为 EventHub 当前只有全局 sender。

- [ ] **Step 8: 实现内部 target wrapper**

把 broadcast channel 的内部消息改为：

```rust
#[derive(Debug, Clone)]
enum PublishedEvent {
    Envelope {
        target_device_id: Option<String>,
        envelope: ServerEnvelope,
    },
    DisconnectDevice {
        device_id: String,
    },
}

#[derive(Debug, Error)]
pub enum EventReceiveError {
    #[error("device was disconnected")]
    DeviceDisconnected,
    #[error("event stream lagged by {0} messages")]
    Lagged(u64),
    #[error("event stream closed")]
    Closed,
}
```

公开 API：

```rust
pub fn publish(&self, envelope: ServerEnvelope) -> usize {
    self.inner.events.send(PublishedEvent::Envelope {
        target_device_id: None,
        envelope,
    }).unwrap_or(0)
}

pub fn publish_to_device(
    &self,
    device_id: impl Into<String>,
    envelope: ServerEnvelope,
) -> usize {
    self.inner.events.send(PublishedEvent::Envelope {
        target_device_id: Some(device_id.into()),
        envelope,
    }).unwrap_or(0)
}

pub fn disconnect_device(&self, device_id: impl Into<String>) -> usize {
    self.inner.events.send(PublishedEvent::DisconnectDevice {
        device_id: device_id.into(),
    }).unwrap_or(0)
}

pub async fn subscribe_for_device(&self, device_id: impl Into<String>) -> EventSubscriber {
    self.subscribe_with_device(Some(device_id.into())).await
}
```

`EventSubscriber::recv` 循环跳过 target 不匹配的 envelope；收到自身 `DisconnectDevice` 时返回 `EventReceiveError::DeviceDisconnected`，WebSocket send loop 随即退出。replay 仍只包含全局 snapshots，不 replay 旧 alert 或 disconnect。

- [ ] **Step 9: WebSocket auth 注入 device ID**

`require_websocket_auth` 改为 `mut request`，认证后插入：

```rust
request.extensions_mut().insert(AuthenticatedDevice { device_id });
```

`ws_handler` 改为：

```rust
async fn ws_handler(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| websocket_stream(state.event_hub, device.device_id, socket))
}
```

`websocket_stream` 使用 `subscribe_for_device`。

- [ ] **Step 10: 运行测试并提交**

Run:

```bash
cargo test -p bridge-core protocol
cargo test -p bridge-core event_hub
cargo test -p bridge-core websocket_route
cd apps/mobile-pwa && npm test -- --run bridge-protocol.test.ts
```

Expected: PASS。

```bash
git add crates/bridge-core/src/protocol.rs crates/bridge-core/src/event_hub.rs crates/bridge-core/src/http_api.rs packages/bridge-protocol/src/protocol.ts apps/mobile-pwa/src/bridge-protocol.test.ts
git commit -m "feat: add targeted alert event protocol"
```

## Task 2: 每设备设置、Public Access 能力与 API

**Files:**
- Create: `crates/bridge-core/src/notification_store.rs`
- Create: `crates/bridge-core/src/public_access.rs`
- Modify: `crates/bridge-core/src/lib.rs`
- Modify: `crates/bridge-core/src/http_api.rs:46-59,153-241,243-341,357-402`
- Modify: `apps/bridge-sidecar/src/main.rs:38-83`
- Modify: `apps/desktop-shell/src-tauri/src/main.rs`
- Create: `apps/mobile-pwa/src/notifications/api.ts`
- Create: `apps/mobile-pwa/src/notifications/api.test.ts`
- Test: `crates/bridge-core/src/notification_store.rs`
- Test: `crates/bridge-core/src/http_api.rs`

- [ ] **Step 1: 写默认设置和完整更新测试**

```rust
#[test]
fn settings_default_disabled_with_all_kinds_preselected() {
    let (_dir, store) = test_store();

    let settings = store.settings_for_device("phone-1").unwrap();

    assert!(!settings.enabled);
    assert!(settings.alert_kinds.completed);
    assert!(settings.alert_kinds.approval_required);
    assert!(settings.alert_kinds.input_required);
    assert!(settings.alert_kinds.error);
    assert!(settings.sound_enabled);
    assert!(settings.vibration_enabled);
}

#[test]
fn settings_replace_the_complete_boolean_document() {
    let (_dir, store) = test_store();
    let settings = DeviceNotificationSettings {
        device_id: "phone-1".into(),
        enabled: true,
        alert_kinds: AlertKindSettings {
            completed: true,
            approval_required: false,
            input_required: true,
            error: false,
        },
        sound_enabled: false,
        vibration_enabled: true,
        updated_at: 10,
    };

    store.save_settings(&settings).unwrap();

    assert_eq!(store.settings_for_device("phone-1").unwrap(), settings);
}
```

- [ ] **Step 2: 运行 store 测试确认失败**

Run:

```bash
cargo test -p bridge-core settings_default_disabled_with_all_kinds_preselected -- --exact
```

Expected: FAIL，因为 NotificationStore 尚不存在。

- [ ] **Step 3: 实现 settings schema 和模型**

`notification_store.rs` 创建：

```sql
CREATE TABLE IF NOT EXISTS device_notification_settings (
    device_id TEXT PRIMARY KEY NOT NULL,
    enabled INTEGER NOT NULL,
    completed_enabled INTEGER NOT NULL,
    approval_required_enabled INTEGER NOT NULL,
    input_required_enabled INTEGER NOT NULL,
    error_enabled INTEGER NOT NULL,
    sound_enabled INTEGER NOT NULL,
    vibration_enabled INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
```

`NotificationStore` 提供 `open(path)` 和 `open_in_memory()`，两者调用同一 notification migration。由于启用查询会 JOIN `devices`，`open_in_memory()` 仅为 `AppState::new` 和单元测试创建一张与当前 `Storage` schema 一致的 in-memory `devices` fixture table；磁盘路径仍由 `Storage::open` 拥有并先完成正式 devices migration。测试 helper 对临时文件同样先调用 `Storage::open`，需要测试启用查询时显式插入 active/revoked device fixture。

公开模型：

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertKindSettings {
    pub completed: bool,
    pub approval_required: bool,
    pub input_required: bool,
    pub error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceNotificationSettings {
    pub device_id: String,
    pub enabled: bool,
    pub alert_kinds: AlertKindSettings,
    pub sound_enabled: bool,
    pub vibration_enabled: bool,
    pub updated_at: u64,
}
```

默认值：master false；四类、sound、vibration true。提供 `enabled_devices()`、`enabled_devices_for_kind()` 和 `any_device_wants_approval_alerts()`，供 monitor/dispatcher 使用。所有“启用设备”查询必须 `JOIN devices` 并要求 `devices.revoked_at IS NULL`；即使撤销后的 cleanup 因数据库错误失败，也不能继续向 revoked device 定向广播。

同时提供幂等的：

```rust
pub fn delete_device_notification_data(&self, device_id: &str) -> Result<()>;
```

Phase 2 先删除该 device 的 settings row；Phase 3 在同一方法内继续扩展 subscription 和 delivery 清理。

- [ ] **Step 4: 写 PublicAccessState 测试**

```rust
#[tokio::test]
async fn named_origin_is_recorded_but_phase_two_delivery_stays_foreground_only() {
    let state = PublicAccessState::default();
    state.update(PublicAccessContext {
        mode: PublicAccessMode::Named,
        public_origin: Some("https://codex.example.com".into()),
    }).await.unwrap();

    let capabilities = state.notification_capabilities().await;
    assert!(capabilities.fixed_https);
    assert_eq!(capabilities.delivery_mode, DeliveryMode::ForegroundOnly);
    assert!(!capabilities.system_notifications);
}
```

- [ ] **Step 5: 实现 PublicAccessState**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicAccessMode { Local, Quick, Named }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicAccessContext {
    pub mode: PublicAccessMode,
    pub public_origin: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    ForegroundOnly,
    WebPush,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionState {
    Unavailable,
    NotEnabled,
    Active,
    NeedsRepair,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationCapabilities {
    pub delivery_mode: DeliveryMode,
    pub fixed_https: bool,
    pub system_notifications: bool,
    pub foreground_sound: bool,
    pub foreground_vibration: bool,
    pub vibration_controlled_by_system: bool,
}

impl Default for PublicAccessContext {
    fn default() -> Self {
        Self {
            mode: PublicAccessMode::Local,
            public_origin: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct PublicAccessState(Arc<RwLock<PublicAccessContext>>);
```

公开 `update(&self, context) -> Result<()>`、`current(&self) -> PublicAccessContext` 和 `notification_capabilities(&self)`。Named 必须是 HTTPS origin；Quick 必须是 HTTPS trycloudflare/public origin；Local 可为 `None`。Phase 2 `delivery_mode` 始终 `foreground_only`，另返回 `fixed_https`，避免在 Web Push 尚未实现时误报锁屏可用。

- [ ] **Step 6: 把 store 与 public access 注入 AppState**

`AppState` 增加：

```rust
notification_store: Arc<Mutex<NotificationStore>>,
public_access: PublicAccessState,
```

新增 builder，保持现有测试默认可使用 in-memory store：

```rust
pub fn with_notification_store(
    mut self,
    store: Arc<Mutex<NotificationStore>>,
) -> Self;
pub fn with_public_access(mut self, public_access: PublicAccessState) -> Self;
```

`AppState::new` 初始化：

```rust
notification_store: Arc::new(Mutex::new(
    NotificationStore::open_in_memory()
        .expect("in-memory notification store initializes"),
)),
public_access: PublicAccessState::default(),
```

sidecar 对同一个 `db_path` 分别打开 `Storage` 和 `NotificationStore`，把后者立即包装为唯一共享的 `Arc<Mutex<NotificationStore>>`，再把同一个 Arc clone 注入 `AppState`、Dispatcher 和后续 Monitor。不要在这些组件中重复打开第二个 `NotificationStore` 实例。

- [ ] **Step 7: 写 Settings API 隔离测试**

测试两个已配对设备，phone A PUT 设置后：

```rust
assert_eq!(get_settings(&app, token_a).await["settings"]["enabled"], true);
assert_eq!(get_settings(&app, token_b).await["settings"]["enabled"], false);
```

再发送缺少 `error` 或非布尔字段的 PUT，Expected: `422 Unprocessable Entity`，数据库保持旧值。

另加 Origin 能力测试：全局 context 为 Named 时，带 `Host: codex.example.com` + `X-Forwarded-Proto: https` 的请求返回 `fixedHttps=true`；直接 `Host: 192.168.1.10:57324` 的 LAN 请求仍返回 `fixedHttps=false`、`foreground_only`。不能因为 Mac 当前运行 Named Tunnel 就让所有入口都误报锁屏可用。

- [ ] **Step 8: 实现 Settings 与 control API**

authenticated routes 增加：

```rust
.route("/api/notification-settings", get(get_notification_settings).put(put_notification_settings))
```

control routes 增加：

```rust
.route("/api/control/remote-access", axum::routing::put(set_public_access_context))
```

`GET` 从 `AuthenticatedDevice.device_id` 读取，返回：

```json
{
  "settings": {
    "enabled": false,
    "alertKinds": {
      "completed": true,
      "approvalRequired": true,
      "inputRequired": true,
      "error": true
    },
    "soundEnabled": true,
    "vibrationEnabled": true
  },
  "capabilities": {
    "deliveryMode": "foreground_only",
    "fixedHttps": true,
    "systemNotifications": false,
    "foregroundSound": true,
    "foregroundVibration": true,
    "vibrationControlledBySystem": false
  },
  "subscriptionState": "unavailable"
}
```

新增 `effective_request_origin(headers)`：优先使用合法的同源 `Origin` header；否则组合 `X-Forwarded-Proto` 的第一个值与 `Host`。结果必须经过与 Phase 1 相同的 http(s) origin normalization。Settings capability 使用 `PublicAccessState.current().public_origin == effective_request_origin` 计算 `fixedHttps`；无法确定 Origin 时保守返回 false。Cloudflare 请求应得到 `https://<public hostname>`，LAN 请求得到 `http://<LAN host:port>`。

`PUT` request struct 使用 `#[serde(deny_unknown_fields)]` 且不使用 `#[serde(default)]`，确保只接受完整、已知的布尔文档。服务端覆盖 `device_id` 和 `updated_at`。

设备撤销 handler 在 pairing revoke 成功后先调用 `event_hub.disconnect_device(device_id)`，再调用 `delete_device_notification_data(device_id)`。新增回归测试：phone A 已打开 WebSocket 且启用提醒，撤销 A 后 socket 关闭，dispatcher 不再向 A publish；phone B 不受影响。cleanup 失败返回 500，但不得回滚已经完成的设备撤销；重复撤销/cleanup 保持幂等。

- [ ] **Step 9: Tauri 在每次模式变化后同步 context**

Named Ready 后 PUT：

```json
{"mode":"named","publicOrigin":"https://codex.example.com"}
```

Quick Ready 后 PUT 当前 Quick origin；停止公网入口后 PUT local。Bridge 每次 start/restart health Ready 后也必须按桌面当前 active mode 重放一次 context，不能只依赖“模式发生变化”事件，否则 sidecar 重启会错误回到 local capability。同步失败显示在 desktop notice/diagnostics，但不伪造 tunnel 成功或失败。

- [ ] **Step 10: 实现 PWA API client 并测试**

`notifications/api.ts` 导出：

```ts
export async function getNotificationSettings(session: DeviceSession): Promise<NotificationSettingsResponse>;
export async function putNotificationSettings(
  session: DeviceSession,
  settings: NotificationSettingsInput,
): Promise<NotificationSettingsResponse>;
```

所有 response 经过显式 type guard；不能用 unchecked cast。测试 Bearer header、完整 PUT body、malformed response rejection。

- [ ] **Step 11: 运行测试并提交**

Run:

```bash
cargo test -p bridge-core notification_store
cargo test -p bridge-core notification_settings
cargo test -p bridge-core public_access
cargo test -p desktop-shell
cd apps/mobile-pwa && npm test -- --run src/notifications/api.test.ts
```

Expected: PASS。

```bash
git add crates/bridge-core/src/notification_store.rs crates/bridge-core/src/public_access.rs crates/bridge-core/src/lib.rs crates/bridge-core/src/http_api.rs apps/bridge-sidecar/src/main.rs apps/desktop-shell/src-tauri/src/main.rs apps/mobile-pwa/src/notifications/api.ts apps/mobile-pwa/src/notifications/api.test.ts
git commit -m "feat: add per-device alert settings"
```

## Task 3: 四类 AlertDetector 与持久化状态

**Files:**
- Create: `crates/bridge-core/src/alert_detector.rs`
- Modify: `crates/bridge-core/src/notification_store.rs`
- Modify: `crates/bridge-core/src/lib.rs`
- Test: `crates/bridge-core/src/alert_detector.rs`
- Test: `crates/bridge-core/src/notification_store.rs`

- [ ] **Step 1: 先写完整状态转换表测试**

测试 helper 固定使用：

```rust
fn snapshot(status: SessionStatus, updated_at: u64) -> SessionSnapshot {
    SessionSnapshot {
        thread_id: "thread-1".into(),
        title: "Release".into(),
        cwd: None,
        model_provider: None,
        preview: None,
        updated_at,
        status,
        pending_approval_ids: Vec::new(),
    }
}
```

至少新增以下测试：

```rust
#[test]
fn first_observation_establishes_baseline_without_alert() {
    let current = snapshot(SessionStatus::Idle, 10);

    let result = detect_alerts(None, &current, &[]);

    assert!(result.events.is_empty());
    assert!(!result.ignored_as_stale);
    assert_eq!(result.next_state.status, SessionStatus::Idle);
    assert_eq!(result.next_state.updated_at, 10);
}

#[test]
fn running_to_idle_emits_completed_once() {
    let previous = SessionAlertState {
        thread_id: "thread-1".into(),
        status: SessionStatus::Running,
        updated_at: 10,
        state_cycle: 0,
        known_approval_ids: Vec::new(),
        fallback_approval_cycle: None,
    };
    let idle = snapshot(SessionStatus::Idle, 20);

    let first = detect_alerts(Some(&previous), &idle, &[]);
    let second = detect_alerts(Some(&first.next_state), &idle, &[]);

    assert_eq!(first.events.len(), 1);
    assert_eq!(first.events[0].kind, AlertKind::Completed);
    assert!(second.events.is_empty());
}

#[test]
fn new_native_approval_id_emits_approval_required() {
    let previous = SessionAlertState {
        thread_id: "thread-1".into(),
        status: SessionStatus::Running,
        updated_at: 10,
        state_cycle: 0,
        known_approval_ids: Vec::new(),
        fallback_approval_cycle: None,
    };

    let result = detect_alerts(
        Some(&previous),
        &snapshot(SessionStatus::Running, 20),
        &["approval-1".into()],
    );

    assert_eq!(result.events.len(), 1);
    assert_eq!(result.events[0].kind, AlertKind::ApprovalRequired);
    assert_eq!(
        result.next_state.known_approval_ids,
        vec!["approval-1".to_string()],
    );
}

#[test]
fn entering_waiting_for_input_emits_input_required_once() {
    let baseline = SessionAlertState {
        thread_id: "thread-1".into(),
        status: SessionStatus::Running,
        updated_at: 10,
        state_cycle: 0,
        known_approval_ids: Vec::new(),
        fallback_approval_cycle: None,
    };
    let first = detect_alerts(
        Some(&baseline),
        &snapshot(SessionStatus::WaitingForInput, 20),
        &[],
    );
    let recovered = detect_alerts(
        Some(&first.next_state),
        &snapshot(SessionStatus::Running, 30),
        &[],
    );
    let second = detect_alerts(
        Some(&recovered.next_state),
        &snapshot(SessionStatus::WaitingForInput, 40),
        &[],
    );

    assert_eq!(first.events[0].kind, AlertKind::InputRequired);
    assert_eq!(second.events[0].kind, AlertKind::InputRequired);
    assert_ne!(first.events[0].event_id, second.events[0].event_id);
}

#[test]
fn entering_error_emits_error_once() {
    let baseline = SessionAlertState {
        thread_id: "thread-1".into(),
        status: SessionStatus::Idle,
        updated_at: 10,
        state_cycle: 0,
        known_approval_ids: Vec::new(),
        fallback_approval_cycle: None,
    };
    let first = detect_alerts(
        Some(&baseline),
        &snapshot(SessionStatus::Error, 20),
        &[],
    );
    let recovered = detect_alerts(
        Some(&first.next_state),
        &snapshot(SessionStatus::Running, 30),
        &[],
    );
    let second = detect_alerts(
        Some(&recovered.next_state),
        &snapshot(SessionStatus::Error, 40),
        &[],
    );

    assert_eq!(first.events[0].kind, AlertKind::Error);
    assert_eq!(second.events[0].kind, AlertKind::Error);
    assert_ne!(first.events[0].event_id, second.events[0].event_id);
}

#[test]
fn older_updated_at_is_ignored_without_replacing_state() {
    let previous = SessionAlertState {
        thread_id: "thread-1".into(),
        status: SessionStatus::Running,
        updated_at: 20,
        state_cycle: 2,
        known_approval_ids: vec!["approval-1".into()],
        fallback_approval_cycle: None,
    };

    let result = detect_alerts(
        Some(&previous),
        &snapshot(SessionStatus::Idle, 10),
        &[],
    );

    assert!(result.ignored_as_stale);
    assert!(result.events.is_empty());
    assert_eq!(result.next_state, previous);
}

#[test]
fn late_native_id_after_fallback_does_not_emit_a_second_approval_alert() {
    let baseline = SessionAlertState {
        thread_id: "thread-1".into(),
        status: SessionStatus::Running,
        updated_at: 10,
        state_cycle: 0,
        known_approval_ids: Vec::new(),
        fallback_approval_cycle: None,
    };
    let fallback = detect_alerts(
        Some(&baseline),
        &snapshot(SessionStatus::WaitingForApproval, 20),
        &[],
    );
    let late_native = detect_alerts(
        Some(&fallback.next_state),
        &snapshot(SessionStatus::WaitingForApproval, 21),
        &["approval-1".into()],
    );

    assert_eq!(fallback.events.len(), 1);
    assert_eq!(fallback.events[0].kind, AlertKind::ApprovalRequired);
    assert!(late_native.events.is_empty());
    assert_eq!(late_native.next_state.known_approval_ids, vec!["approval-1"]);
}
```

- [ ] **Step 2: 运行 detector 测试确认失败**

Run:

```bash
cargo test -p bridge-core alert_detector -- --nocapture
```

Expected: FAIL，因为 detector 尚未实现。

- [ ] **Step 3: 扩展 session_alert_state schema**

```sql
CREATE TABLE IF NOT EXISTS session_alert_state (
    thread_id TEXT PRIMARY KEY NOT NULL,
    status TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    state_cycle INTEGER NOT NULL,
    known_approval_ids_json TEXT NOT NULL,
    fallback_approval_cycle INTEGER
);
```

公开模型：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAlertState {
    pub thread_id: String,
    pub status: SessionStatus,
    pub updated_at: u64,
    pub state_cycle: u64,
    pub known_approval_ids: Vec<String>,
    pub fallback_approval_cycle: Option<u64>,
}
```

提供 `alert_state_for_thread`、`save_alert_state` 和 transaction 批量保存。JSON 解析失败返回错误，不静默重置为 baseline。

- [ ] **Step 4: 实现稳定 event ID**

```rust
fn stable_event_id(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    format!("alert-{:x}", hasher.finalize())
}
```

ID key：

- completed: kind + thread + previous running `updated_at` + current idle `updated_at`
- input/error/fallback approval: kind + thread + new `state_cycle`
- native approval: kind + thread + approval ID

同一输入在 Bridge 重启后必须得到同一 event ID。

- [ ] **Step 5: 实现 detector 纯函数**

公开接口：

```rust
pub struct DetectionResult {
    pub next_state: SessionAlertState,
    pub events: Vec<AlertEvent>,
    pub ignored_as_stale: bool,
}

pub fn detect_alerts(
    previous: Option<&SessionAlertState>,
    snapshot: &SessionSnapshot,
    native_approval_ids: &[String],
) -> DetectionResult;
```

关键顺序：

```rust
if let Some(previous) = previous {
    if snapshot.updated_at < previous.updated_at {
        return DetectionResult::stale(previous.clone());
    }
}

let first_observation = previous.is_none();
let status_changed = previous.is_some_and(|state| state.status != snapshot.status);
let state_cycle = previous.map_or(0, |state| {
    state.state_cycle + if status_changed { 1 } else { 0 }
});
```

首次 observation 只建立 baseline，并把当前 native approval IDs 纳入 known set。后续先对 native ID 做差集，再处理 status transition；当存在 native approval event 时，不额外生成 fallback waiting_for_approval event。生成 fallback event 时把当前 `state_cycle` 写入 `fallback_approval_cycle`；离开 `WaitingForApproval` 时把该字段清回 `None`。如果下一轮仍处于同一 waiting cycle、previous known IDs 为空且原生 ID 才晚到，则只把这批 ID 吸收到 known set，不再生成第二条 approval event；吸收后同一 cycle 后续出现的其他新 ID 仍按 native approval 正常提醒。

状态事件条件必须逐字实现为：

```rust
let completed = previous.is_some_and(|state| {
    state.status == SessionStatus::Running && snapshot.status == SessionStatus::Idle
});
let input_required = previous.is_some_and(|state| {
    state.status != SessionStatus::WaitingForInput
        && snapshot.status == SessionStatus::WaitingForInput
});
let error = previous.is_some_and(|state| {
    state.status != SessionStatus::Error && snapshot.status == SessionStatus::Error
});
let fallback_approval = previous.is_some_and(|state| {
    state.status != SessionStatus::WaitingForApproval
        && snapshot.status == SessionStatus::WaitingForApproval
}) && new_native_approval_ids.is_empty();
```

同一个 cycle 如果既有 native approval 又有 input/error 状态变化，可以分别生成对应事件；只有 fallback approval 会被 native approval 抑制。

所有生成的 `AlertEvent` 统一使用 `snapshot.thread_id`、`snapshot.title` 和 `snapshot.updated_at` 填充 `thread_id`、`thread_title`、`occurred_at`；不得把回复正文、CWD、approval detail 或 error detail 放入事件。fallback approval 仅在进入 `WaitingForApproval` 且当前没有新 native approval ID 时生成。

`known_approval_ids` 去重并按首次出现顺序保留，最多 256 个；超限删除最旧且当前不 pending 的 ID。

- [ ] **Step 6: 写 SQLite 重启恢复测试**

```rust
#[test]
fn persisted_alert_state_prevents_duplicate_after_store_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bridge.sqlite");
    let store = NotificationStore::open(&path).unwrap();
    store.save_alert_state(&state_waiting_for_input()).unwrap();
    drop(store);

    let reopened = NotificationStore::open(&path).unwrap();
    let previous = reopened.alert_state_for_thread("thread-1").unwrap().unwrap();
    let result = detect_alerts(Some(&previous), &snapshot(SessionStatus::WaitingForInput, 20), &[]);

    assert!(result.events.is_empty());
}
```

- [ ] **Step 7: 运行 detector/store 测试**

Run:

```bash
cargo test -p bridge-core alert_detector
cargo test -p bridge-core persisted_alert_state_prevents_duplicate_after_store_reopen -- --exact
```

Expected: PASS。

- [ ] **Step 8: 提交**

```bash
git add crates/bridge-core/src/alert_detector.rs crates/bridge-core/src/notification_store.rs crates/bridge-core/src/lib.rs
git commit -m "feat: detect persistent task alert transitions"
```

## Task 4: AlertMonitor、自适应轮询和定向 Dispatcher

**Files:**
- Create: `crates/bridge-core/src/notification_dispatcher.rs`
- Create: `crates/bridge-core/src/alert_monitor.rs`
- Modify: `crates/bridge-core/src/lib.rs`
- Modify: `crates/bridge-core/src/http_api.rs`
- Modify: `apps/bridge-sidecar/src/main.rs`
- Test: `crates/bridge-core/src/notification_dispatcher.rs`
- Test: `crates/bridge-core/src/alert_monitor.rs`
- Test: `crates/bridge-core/src/http_api.rs`

- [ ] **Step 1: 写按设置过滤的 dispatcher 测试**

```rust
#[tokio::test]
async fn dispatcher_targets_only_devices_with_master_and_kind_enabled() {
    let (_dir, store) = test_store();
    store.save_settings(&settings("phone-a", true, true)).unwrap();
    store.save_settings(&settings("phone-b", true, false)).unwrap();
    store.save_settings(&settings("phone-c", false, true)).unwrap();
    let store = Arc::new(Mutex::new(store));
    let hub = EventHub::new();
    let mut phone_a = hub.subscribe_for_device("phone-a").await;
    let mut phone_b = hub.subscribe_for_device("phone-b").await;
    let dispatcher = NotificationDispatcher::new(store, hub);

    dispatcher.dispatch(alert(AlertKind::Completed)).await.unwrap();

    assert!(matches!(phone_a.recv().await.unwrap(), ServerEnvelope::AlertEvent(_)));
    assert!(timeout(Duration::from_millis(25), phone_b.recv()).await.is_err());
}
```

- [ ] **Step 2: 运行 dispatcher 测试确认失败**

Run:

```bash
cargo test -p bridge-core dispatcher_targets_only_devices_with_master_and_kind_enabled -- --exact
```

Expected: FAIL，因为 dispatcher 尚不存在。

- [ ] **Step 3: 实现 NotificationDispatcher**

```rust
#[derive(Clone)]
pub struct NotificationDispatcher {
    store: Arc<Mutex<NotificationStore>>,
    event_hub: EventHub,
}

impl NotificationDispatcher {
    pub fn new(store: Arc<Mutex<NotificationStore>>, event_hub: EventHub) -> Self {
        Self { store, event_hub }
    }

    pub async fn dispatch(&self, event: AlertEvent) -> anyhow::Result<usize> {
        let targets = self.store.lock().await.enabled_devices_for_kind(event.kind)?;
        let mut deliveries = 0;
        for device_id in targets {
            deliveries += self.event_hub.publish_to_device(
                device_id,
                ServerEnvelope::AlertEvent(event.clone()),
            );
        }
        Ok(deliveries)
    }

    pub async fn dispatch_test_to_device(
        &self,
        device_id: &str,
        event: AlertEvent,
    ) -> usize {
        self.event_hub.publish_to_device(
            device_id.to_string(),
            ServerEnvelope::AlertEvent(event),
        )
    }
}
```

普通 dispatch 严格应用 master + kind；显式测试提醒只发当前 device，允许用户在 master 关闭时验证 UI。

- [ ] **Step 4: 写 monitor 单周期测试**

测试 adapter 返回两个 thread 和一个 native approval：

```rust
#[tokio::test]
async fn monitor_combines_threads_and_native_approvals_in_one_cycle() {
    let adapter = Arc::new(TestAdapter::new(
        vec![thread("thread-1", "running", 10)],
        vec![pending_approval("thread-1", "approval-1")],
    ));
    let harness = MonitorHarness::new(adapter.clone());
    harness.seed_state(state("thread-1", SessionStatus::Running, 9)).await;

    let outcome = harness.monitor.run_cycle().await.unwrap();

    assert_eq!(adapter.thread_calls(), 1);
    assert_eq!(adapter.approval_calls(), 1);
    assert_eq!(outcome.next_delay, Duration::from_secs(5));
    assert_eq!(harness.received_kinds("phone-1").await, vec![AlertKind::ApprovalRequired]);
}
```

另加：全部 idle => 30 秒；无启用设备 => 不调用 adapter、30 秒；adapter error => 状态不写入且退避 5/10/20/30 秒；approval 类型全关 => 不调用 `list_pending_approvals`。

- [ ] **Step 5: 运行 monitor 测试确认失败**

Run:

```bash
cargo test -p bridge-core alert_monitor -- --nocapture
```

Expected: FAIL，因为 monitor 尚不存在。

- [ ] **Step 6: 实现 AlertMonitor 配置和单周期**

```rust
#[derive(Debug, Clone)]
pub struct AlertMonitorConfig {
    pub active_poll: Duration,
    pub idle_poll: Duration,
    pub max_error_backoff: Duration,
}

impl Default for AlertMonitorConfig {
    fn default() -> Self {
        Self {
            active_poll: Duration::from_secs(5),
            idle_poll: Duration::from_secs(30),
            max_error_backoff: Duration::from_secs(30),
        }
    }
}
```

`run_cycle` 固定顺序：

```rust
let enabled = self.store.lock().await.enabled_settings()?;
if enabled.is_empty() {
    self.consecutive_failures = 0;
    return Ok(MonitorCycleOutcome::idle(self.config.idle_poll));
}

let threads = self.adapter.list_threads().await?;
let approvals = if enabled.iter().any(|settings| settings.alert_kinds.approval_required) {
    self.adapter.list_pending_approvals().await?
} else {
    Vec::new()
};

let approvals_by_thread = approvals.into_iter().fold(HashMap::new(), |mut map, item| {
    map.entry(item.thread_id).or_insert_with(Vec::new).push(item.request_id);
    map
});
let snapshots = threads.iter().map(Normalizer::snapshot_from_thread).collect::<Vec<_>>();
```

每个 snapshot：读取 previous；调用 `detect_alerts`；stale 不写；否则先持久化 `next_state`，再逐个 dispatch。adapter 任一读取失败时整个周期不调用 detector、不修改状态、不推断 transition。

active 状态集合：Running、WaitingForInput、WaitingForApproval、Error。全部 Idle 使用 30 秒。

- [ ] **Step 7: 实现有限指数退避 run loop**

```rust
fn monitor_error_category(error: &anyhow::Error) -> &'static str {
    if let Some(error) = error.downcast_ref::<CodexRpcError>() {
        return match error {
            CodexRpcError::Transport(_) => "adapter_transport",
            CodexRpcError::InvalidResponse { .. } => "adapter_invalid_response",
            CodexRpcError::Unsupported { .. } => "adapter_unsupported",
        };
    }
    if error.downcast_ref::<rusqlite::Error>().is_some() {
        return "notification_store";
    }
    "alert_monitor"
}

pub async fn run(mut self) {
    loop {
        let delay = match self.run_cycle().await {
            Ok(outcome) => {
                self.consecutive_failures = 0;
                outcome.next_delay
            }
            Err(error) => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                let seconds = 5u64
                    .saturating_mul(1u64 << self.consecutive_failures.saturating_sub(1).min(3));
                eprintln!(
                    "alert monitor cycle failed: {}",
                    monitor_error_category(&error),
                );
                Duration::from_secs(seconds).min(self.config.max_error_backoff)
            }
        };
        sleep(delay).await;
    }
}
```

日志只输出错误类别，不输出 thread 内容、approval detail 或 token。

- [ ] **Step 8: 实现当前设备测试提醒 API**

authenticated route 增加：

```rust
.route("/api/notifications/test", post(send_test_notification))
```

handler：

```rust
async fn send_test_notification(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
) -> Result<(StatusCode, Json<AlertEvent>), ApiError> {
    let now = current_time_ms();
    let event = AlertEvent {
        event_id: format!("test-alert-{}", Uuid::new_v4()),
        kind: AlertKind::Completed,
        thread_id: "notification-test".to_string(),
        thread_title: "Codex Mobile Bridge".to_string(),
        occurred_at: now,
    };
    state.notification_dispatcher
        .dispatch_test_to_device(&device.device_id, event.clone())
        .await;
    Ok((StatusCode::ACCEPTED, Json(event)))
}
```

测试两个 device WebSocket，POST with phone A token 后只有 A 收到。

- [ ] **Step 9: Sidecar 启动 monitor**

共享同一个 `EventHub`、`Arc<Mutex<NotificationStore>>` 和 dispatcher：

```rust
let event_hub = EventHub::new();
let notification_store = Arc::new(tokio::sync::Mutex::new(
    NotificationStore::open(&db_path).context("open notification storage")?,
));
let dispatcher = NotificationDispatcher::new(
    Arc::clone(&notification_store),
    event_hub.clone(),
);
let mut state = AppState::new(pairing, event_hub, control_token)
    .with_notification_store(Arc::clone(&notification_store))
    .with_notification_dispatcher(dispatcher.clone())
    .with_diagnostics(diagnostics);

if let Some(adapter) = codex_adapter {
    tokio::spawn(AlertMonitor::new(
        Arc::clone(&adapter),
        Arc::clone(&notification_store),
        dispatcher,
        AlertMonitorConfig::default(),
    ).run());
    state = state.with_codex_adapter(adapter);
}
```

保留 `db_path` clone，避免先 move 给 `Storage::open`。

- [ ] **Step 10: 运行 Rust 测试并提交**

Run:

```bash
cargo test -p bridge-core notification_dispatcher
cargo test -p bridge-core alert_monitor
cargo test -p bridge-core notifications_test
cargo test -p bridge-sidecar
```

Expected: PASS。

```bash
git add crates/bridge-core/src/notification_dispatcher.rs crates/bridge-core/src/alert_monitor.rs crates/bridge-core/src/lib.rs crates/bridge-core/src/http_api.rs apps/bridge-sidecar/src/main.rs
git commit -m "feat: monitor and dispatch task alerts"
```

## Task 5: PWA Settings API、能力检测和四种前台提示

**Files:**
- Create: `apps/mobile-pwa/src/notifications/capabilities.ts`
- Create: `apps/mobile-pwa/src/notifications/capabilities.test.ts`
- Create: `apps/mobile-pwa/src/notifications/foreground-alert-player.ts`
- Create: `apps/mobile-pwa/src/notifications/foreground-alert-player.test.ts`
- Modify: `apps/mobile-pwa/src/notifications/api.ts`
- Modify: `apps/mobile-pwa/src/notifications/api.test.ts`

- [ ] **Step 1: 写平台能力检测测试**

```ts
it("reports foreground-only and unsupported vibration honestly", () => {
  const capabilities = detectForegroundCapabilities({
    fixedHttps: false,
    hasAudioContext: true,
    hasVibrate: false,
  });

  expect(capabilities).toEqual({
    deliveryMode: "foreground_only",
    fixedHttps: false,
    foregroundSound: true,
    foregroundVibration: false,
    vibrationControlledBySystem: false,
    lockScreenMessage: "Lock-screen alerts require a fixed HTTPS address",
  });
});
```

- [ ] **Step 2: 实现能力模型**

`capabilities.ts` 不依赖 User-Agent 作为唯一判断，使用 feature detection：

```ts
export function browserForegroundCapabilities(fixedHttps: boolean): ForegroundCapabilities {
  return detectForegroundCapabilities({
    fixedHttps,
    hasAudioContext: "AudioContext" in window || "webkitAudioContext" in window,
    hasVibrate: typeof navigator.vibrate === "function",
  });
}
```

本阶段 `deliveryMode` 总是 `foreground_only`。固定 HTTPS 只显示“固定地址已就绪；锁屏系统通知将在 Web Push 启用后可用”，临时模式明确要求固定地址。

- [ ] **Step 3: 写声音映射、去重和显式试听测试**

```ts
it("maps every alert kind to a distinct preset tone", () => {
  expect(new Set(Object.values(ALERT_TONES).map((tone) => JSON.stringify(tone))).size).toBe(4);
});

it("plays and vibrates one foreground alert only once per event id", async () => {
  const tone = new RecordingToneEngine();
  const vibration = vi.fn();
  const player = new ForegroundAlertPlayer(tone, vibration, () => "visible");
  const settings = enabledSettings();

  await player.handle(alert("event-1", "approval_required"), settings);
  await player.handle(alert("event-1", "approval_required"), settings);

  expect(tone.played).toEqual(["approval_required"]);
  expect(vibration).toHaveBeenCalledTimes(1);
});

it("preview_plays_even_when_global_sound_is_disabled", async () => {
  const tone = new RecordingToneEngine();
  const player = new ForegroundAlertPlayer(tone, vi.fn(), () => "visible");

  await player.preview("error");

  expect(tone.played).toEqual(["error"]);
});
```

- [ ] **Step 4: 运行 player 测试确认失败**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run src/notifications/foreground-alert-player.test.ts
```

Expected: FAIL，因为 player 尚不存在。

- [ ] **Step 5: 实现四种内置 Web Audio tone**

```ts
export const ALERT_TONES: Record<AlertKind, readonly ToneStep[]> = {
  completed: [
    { frequency: 659, durationMs: 80, gapMs: 25 },
    { frequency: 880, durationMs: 110, gapMs: 0 },
  ],
  approval_required: [
    { frequency: 523, durationMs: 90, gapMs: 70 },
    { frequency: 659, durationMs: 90, gapMs: 0 },
  ],
  input_required: [
    { frequency: 740, durationMs: 65, gapMs: 45 },
    { frequency: 740, durationMs: 65, gapMs: 0 },
  ],
  error: [
    { frequency: 392, durationMs: 120, gapMs: 45 },
    { frequency: 311, durationMs: 150, gapMs: 0 },
  ],
};
```

真实 `WebAudioToneEngine` 每个 oscillator 使用 `gain.maxValue <= 0.08`，做 8ms attack/20ms release，不循环。`unlock()` 只能从用户点击 Enable/试听调用。

震动模式：

```ts
export const ALERT_VIBRATION: Record<AlertKind, readonly number[]> = {
  completed: [80],
  approval_required: [80, 60, 80],
  input_required: [45, 40, 45],
  error: [150, 80, 150],
};
```

- [ ] **Step 6: 实现 ForegroundAlertPlayer 过滤和 LRU 去重**

```ts
export class ForegroundAlertPlayer {
  private readonly seen = new Map<string, number>();

  async handle(event: AlertEvent, settings: DeviceNotificationSettings): Promise<AlertPlaybackResult> {
    if (this.seen.has(event.eventId)) return { played: false, duplicate: true };
    this.remember(event.eventId);
    if (this.visibility() !== "visible" || !settings.enabled || !isKindEnabled(settings, event.kind)) {
      return { played: false, duplicate: false };
    }
    let soundBlocked = false;
    if (settings.soundEnabled) {
      try { await this.tone.play(event.kind); } catch { soundBlocked = true; }
    }
    if (settings.vibrationEnabled) this.vibrate([...ALERT_VIBRATION[event.kind]]);
    return { played: true, duplicate: false, soundBlocked };
  }
}
```

LRU 最多 256 个 event ID；超出删除最旧。音频失败只返回一次 `soundBlocked` 提示，不抛到全局错误状态。

- [ ] **Step 7: 扩展 API client 的 test alert**

增加：

```ts
export async function sendTestAlert(session: DeviceSession): Promise<AlertEvent>;
```

测试 `POST /api/notifications/test`、Bearer header 和严格 AlertEvent parsing。

- [ ] **Step 8: 运行前端模块测试并提交**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run src/notifications
```

Expected: PASS。

```bash
git add apps/mobile-pwa/src/notifications/capabilities.ts apps/mobile-pwa/src/notifications/capabilities.test.ts apps/mobile-pwa/src/notifications/foreground-alert-player.ts apps/mobile-pwa/src/notifications/foreground-alert-player.test.ts apps/mobile-pwa/src/notifications/api.ts apps/mobile-pwa/src/notifications/api.test.ts
git commit -m "feat: add configurable foreground alert playback"
```

## Task 6: 完整 Settings 页面与首次启用引导

**Files:**
- Create: `apps/mobile-pwa/src/notifications/NotificationSettingsPage.tsx`
- Create: `apps/mobile-pwa/src/notifications/NotificationSettingsPage.test.tsx`
- Create: `apps/mobile-pwa/src/notifications/NotificationOnboardingSheet.tsx`
- Create: `apps/mobile-pwa/src/notifications/NotificationOnboardingSheet.test.tsx`
- Create: `apps/mobile-pwa/src/notifications/onboarding-storage.ts`
- Modify: `apps/mobile-pwa/src/App.tsx:107-168,287-368,574-591,821-890,956-1045,1179-1253,2783-2810`
- Modify: `apps/mobile-pwa/src/App.test.tsx`
- Modify: `apps/mobile-pwa/src/styles.css`

- [ ] **Step 1: 写 Settings 组件行为测试**

```tsx
it("renders master, four kinds, sound, vibration, connection and test controls", () => {
  render(<NotificationSettingsPage {...defaultProps()} />);

  expect(screen.getByRole("switch", { name: "Task alerts" })).toBeInTheDocument();
  expect(screen.getByRole("switch", { name: "Completed alerts" })).toBeInTheDocument();
  expect(screen.getByRole("switch", { name: "Approval required alerts" })).toBeInTheDocument();
  expect(screen.getByRole("switch", { name: "Input required alerts" })).toBeInTheDocument();
  expect(screen.getByRole("switch", { name: "Error alerts" })).toBeInTheDocument();
  expect(screen.getByRole("switch", { name: "Sound" })).toBeInTheDocument();
  expect(screen.getByRole("switch", { name: "Vibration" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Send test alert" })).toBeInTheDocument();
});

it("preview_calls_the_selected_tone_even_when_sound_switch_is_off", async () => {
  const onPreview = vi.fn();
  render(<NotificationSettingsPage {...defaultProps({ soundEnabled: false, onPreview })} />);
  await userEvent.click(screen.getByRole("button", { name: "Preview error sound" }));
  expect(onPreview).toHaveBeenCalledWith("error");
});
```

- [ ] **Step 2: 写首次引导只展示一次测试**

```tsx
it("not_now_dismisses_onboarding_for_the_current_device", async () => {
  render(<NotificationOnboardingSheet deviceId="phone-1" {...props()} />);
  await userEvent.click(screen.getByRole("button", { name: "Not now" }));
  expect(onDismiss).toHaveBeenCalled();
  expect(hasDismissedNotificationOnboarding("phone-1")).toBe(true);
});
```

另测临时模式文案包含 `foreground only`；固定模式不在 Phase 2 请求 `Notification.requestPermission`。

- [ ] **Step 3: 运行组件测试确认失败**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run src/notifications/NotificationSettingsPage.test.tsx src/notifications/NotificationOnboardingSheet.test.tsx
```

Expected: FAIL，因为组件尚不存在。

- [ ] **Step 4: 实现 onboarding 本地状态**

```ts
const PREFIX = "codex.mobilePwa.notificationOnboarding.v1";

export function onboardingStorageKey(deviceId: string): string {
  return `${PREFIX}:${deviceId}`;
}

export function dismissNotificationOnboarding(deviceId: string): void {
  localStorage.setItem(onboardingStorageKey(deviceId), "dismissed");
}

export function hasDismissedNotificationOnboarding(deviceId: string): boolean {
  return localStorage.getItem(onboardingStorageKey(deviceId)) === "dismissed";
}
```

选择 Enable 或 Not now 都写 dismissed，避免每次刷新重复弹出。

- [ ] **Step 5: 实现 Settings 页面**

页面结构为 full-width sections，不嵌套 card：

- 顶栏：Back 按钮 + `Settings`。
- Notifications：master switch 和当前状态。
- Alert types：四行，每行 icon、名称、switch、耳机/播放 icon button。
- Delivery：Sound、Vibration、Send test alert。
- Connection：Fixed HTTPS / Temporary、当前 public origin、锁屏能力说明。

所有更新发送完整 settings object；PUT 进行中禁用 switches；失败回滚到服务端最后成功值并显示 inline error。

- [ ] **Step 6: 实现首次启用引导**

标题和操作固定：

```tsx
<h2>Get notified when Codex needs you</h2>
<button type="button" onClick={onEnable}>Enable alerts</button>
<button type="button" onClick={onNotNow}>Not now</button>
```

Enable 执行：

1. `player.unlock()`；
2. PUT master true，四类 true，sound/vibration true；
3. dismiss onboarding；
4. `sendTestAlert`；
5. 本阶段不调用 Notification permission 或 PushManager。

- [ ] **Step 7: App 增加 Settings 导航且保留 workbench 状态**

增加：

```ts
type AppView = "workbench" | "settings";
const [view, setView] = useState<AppView>("workbench");
```

不要条件卸载 `SessionDetail`。使用：

```tsx
<div hidden={view !== "workbench"}>{workbenchView}</div>
<div hidden={view !== "settings"}>
  {notificationSettingsPage}
</div>
```

把现有 workbench JSX 原样提取为局部常量 `workbenchView`，把带完整 props 的 `NotificationSettingsPage` 提取为 `notificationSettingsPage`；两者都在每次 render 中构造，但只通过 `hidden` 切换可见性。这样返回后 selected thread、已加载 events 和 scroll container DOM 保留。Session drawer heading 增加 lucide `Settings` icon button，点击先关闭 drawer 再切 view。

- [ ] **Step 8: App 接收 alert_event 并统一交给 player**

把 `handleServerEnvelope` 增加 callback：

```ts
function handleServerEnvelope(
  envelope: ServerEnvelope,
  setLiveSessions: Dispatch<SetStateAction<SessionSnapshot[] | null>>,
  setEventsByThread: Dispatch<SetStateAction<Record<string, SessionEvent[]>>>,
  setApprovals: Dispatch<SetStateAction<ApprovalRequest[]>>,
  onAlert: (event: AlertEvent) => void,
): void {
  switch (envelope.type) {
    case "alert_event":
      onAlert(envelope.payload);
      break;
    // existing cases unchanged
  }
}
```

`onAlert` 调用同一个 `ForegroundAlertPlayer` ref 和当前 settings ref；若返回 `soundBlocked`，只显示一次 `Tap to enable sound` 可点击提示。

- [ ] **Step 9: 增加 App 集成测试**

覆盖：

- drawer Settings 按钮进入页面，Back 恢复原 selected session；
- WebSocket `alert_event` 只触发一次 tone；相同 eventId 第二次忽略；
- kind 关闭时不播放；master 关闭时不播放；
- temporary connection 文案明确锁屏不可用；
- 首次配对只出现一次 onboarding；Not now 后刷新不再出现。

- [ ] **Step 10: 样式与移动端布局**

Settings 不能挤占 composer；在手机上占满 workbench 到 viewport bottom，自身滚动。switch 使用稳定宽度；四个最长标签可换行但不能覆盖 preview button。所有 icon button 提供 `aria-label` 和 tooltip/title。

- [ ] **Step 11: 运行 PWA 测试/build 并提交**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run
npm run build
```

Expected: PASS；现有发送消息、附件、审批、会话滚动测试无回归。

```bash
git add apps/mobile-pwa/src/notifications/NotificationSettingsPage.tsx apps/mobile-pwa/src/notifications/NotificationSettingsPage.test.tsx apps/mobile-pwa/src/notifications/NotificationOnboardingSheet.tsx apps/mobile-pwa/src/notifications/NotificationOnboardingSheet.test.tsx apps/mobile-pwa/src/notifications/onboarding-storage.ts apps/mobile-pwa/src/App.tsx apps/mobile-pwa/src/App.test.tsx apps/mobile-pwa/src/styles.css
git commit -m "feat: add mobile alert settings experience"
```

## Task 7: Phase 2 版本、回归与人工前台提醒 QA

**Files:**
- Modify: `docs/dogfood-qa-checklist.md`
- Modify: `VERSION`
- Modify: `crates/bridge-core/Cargo.toml`
- Modify: `crates/desktop-core/Cargo.toml`
- Modify: `apps/bridge-sidecar/Cargo.toml`
- Modify: `apps/desktop-shell/src-tauri/Cargo.toml`
- Modify: `apps/desktop-shell/src-tauri/tauri.conf.json`
- Modify: `apps/desktop-shell/package.json`
- Modify: `apps/desktop-shell/package-lock.json`
- Modify: `apps/mobile-pwa/package.json`
- Modify: `apps/mobile-pwa/package-lock.json`

- [ ] **Step 1: 更新 dogfood 场景**

```markdown
- [ ] 首次配对只出现一次提醒引导；Not now 后刷新不再弹出。
- [ ] master 或对应 kind 关闭后，事件不产生声音/震动。
- [ ] completed、approval、input、error 四种前台音效可明显区分且响度接近。
- [ ] Settings 试听在 Sound 关闭时仍可播放。
- [ ] 页面前台收到同一 eventId 的重复 WS 消息时只播放一次。
- [ ] Bridge 重启后持续 waiting/error 状态不重复提醒。
- [ ] 临时通道明确显示锁屏提醒不可用。
- [ ] 手机不支持 vibration 时开关禁用并说明，不伪装成功。
```

- [ ] **Step 2: 统一升级版本到 0.1.6**

更新 `VERSION`、所有内部 Cargo/package/Tauri 版本及 package-lock 顶层版本。

Run:

```bash
scripts/check-version-sync.sh
```

Expected: `Version 0.1.6 is synchronized across desktop, sidecar, and PWA manifests.`

- [ ] **Step 3: 运行完整自动回归**

Run:

```bash
cargo fmt --all -- --check
cargo test --workspace
cd apps/mobile-pwa && npm test -- --run && npm run build
cd ../desktop-shell && npm test -- --run && npm run build
cd ../.. && scripts/check-release-gate.sh --channel dev
```

Expected: 全部 PASS。

- [ ] **Step 4: 浏览器和真机前台 QA**

在 LAN、Named Tunnel、Quick Tunnel 各测试一次：保持页面可见，分别制造 running->idle、真实 approval、waiting_for_input、error；确认 5 秒轮询下 15 秒内提醒，且四种声音不同。页面切后台时本阶段不承诺系统通知，UI 必须明确说明。

- [ ] **Step 5: 构建 v0.1.6 DMG**

Run:

```bash
cd apps/desktop-shell && npm run tauri:build
```

Expected: DMG 内 desktop、sidecar、PWA 都显示 `0.1.6`。

- [ ] **Step 6: 提交 Phase 2**

```bash
git add docs/dogfood-qa-checklist.md VERSION crates/bridge-core/Cargo.toml crates/desktop-core/Cargo.toml apps/bridge-sidecar/Cargo.toml apps/desktop-shell/src-tauri/Cargo.toml apps/desktop-shell/src-tauri/tauri.conf.json apps/desktop-shell/package.json apps/desktop-shell/package-lock.json apps/mobile-pwa/package.json apps/mobile-pwa/package-lock.json
git commit -m "release: prepare foreground alerts v0.1.6"
```

## Phase 2 验收门槛

- 首次 observation 不提醒；四类 transition、恢复后再次进入、native approval ID 和乱序 snapshot 全部有单元测试。
- Bridge 重启读取 `session_alert_state`，持续状态不重复提醒。
- monitor 监控全部会话；有 active 状态 5 秒，全部 idle/无启用设备 30 秒；adapter 失败最大退避 30 秒且不推断状态。
- Settings 按设备隔离；master 和四类 kind 均由服务端 dispatcher 实际过滤，不只是前端隐藏。
- 撤销设备会定向关闭其现有 WebSocket，并从启用设备查询中排除，不能继续接收普通会话流或提醒。
- 同一 eventId 在 PWA 前台最多播放一次；四类 tone 和震动模式不同；试听不受 Sound 全局开关影响。
- Settings 独立页面不占用消息区，返回后保留 selected session 和滚动状态。
- Quick Tunnel 始终显示 foreground only；Phase 2 不请求 Notification permission，不声称支持锁屏。
- Rust workspace、PWA、desktop-shell 测试与 build、版本同步、dev release gate 全部通过。
