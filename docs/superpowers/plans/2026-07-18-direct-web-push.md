# 直接 Web Push 与锁屏通知 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在固定 HTTPS Origin 下为每台已配对手机建立直接 Web Push，使 PWA 进入后台或锁屏后仍能收到四类系统通知，并与前台 WebSocket 提醒按同一 eventId 去重。

**Architecture:** Mac App 在 Keychain 中生成并保存 VAPID 私钥，通过一次性 `0600` secret file 交给 Bridge sidecar；sidecar 持久化 PushSubscription 和 delivery outbox，使用 `web-push` 发送并分类重试。PWA 使用构建后的 TypeScript Service Worker 处理 push、可见页面转发、系统通知、点击 deep-link 和 IndexedDB 去重。

**Tech Stack:** Rust 2024、keyring、p256、web-push 0.11、Tokio、SQLite outbox、React 19、Push API、Service Worker、IndexedDB、Vitest、fake-indexeddb、Vite multi-entry build。

---

## 文件结构

- Create `crates/desktop-core/src/vapid_key.rs`: VAPID P-256 key 生成、Keychain 读取和 public key 派生。
- Create `crates/desktop-core/src/secret_file.rs`: 通用 `0600` 一次性 secret file；Phase 1 Named Tunnel 迁移到该模块。
- Modify `crates/desktop-core/src/bridge_process.rs`: 支持额外非敏感 env 和启动失败 secret cleanup。
- Modify `apps/desktop-shell/src-tauri/src/main.rs`: Bridge 启动前准备 VAPID secret file。
- Modify `apps/bridge-sidecar/src/main.rs`: 读取并立即删除 VAPID secret file，构造 WebPush runtime。
- Create `crates/bridge-core/src/vapid.rs`: sidecar 内存 VAPID key material 和 public key。
- Extend `crates/bridge-core/src/notification_store.rs`: PushSubscription 与 delivery outbox schema。
- Create `crates/bridge-core/src/web_push.rs`: payload、web-push transport、错误分类和发送。
- Create `crates/bridge-core/src/push_delivery_worker.rs`: outbox claim、有限重试、重启恢复和失效订阅处理。
- Modify `crates/bridge-core/src/notification_dispatcher.rs`: 固定模式 enqueue Web Push，同时保留定向 WebSocket。
- Modify `crates/bridge-core/src/http_api.rs`: public key、subscription 注册/删除、真实测试通知和 Settings 状态。
- Create `apps/mobile-pwa/src/notifications/push-protocol.ts`: push payload 严格解析和通知文案。
- Create `apps/mobile-pwa/src/notifications/recent-event-store.ts`: IndexedDB eventId TTL/LRU 去重。
- Create `apps/mobile-pwa/src/service-worker.ts`: cache、push、notificationclick 和 client message。
- Modify `apps/mobile-pwa/vite.config.ts`: 把 Service Worker 构建为稳定 `/sw.js`。
- Create `apps/mobile-pwa/tsconfig.sw.json`: 用 WebWorker lib 单独类型检查 Service Worker，避免和页面 DOM globals 冲突。
- Delete `apps/mobile-pwa/public/sw.js`: 避免静态 worker 与构建 worker 并存。
- Create `apps/mobile-pwa/src/notifications/push-subscription-controller.ts`: permission、subscribe、repair、unsubscribe。
- Modify Settings、onboarding、App 和 styles：接入系统通知状态、安装指引和通知 deep-link。
- Modify diagnostics、dogfood/release docs、版本清单：完成 Phase 3 后统一升至 `0.1.7`。

## Task 1: VAPID Keychain 生命周期与 Sidecar 安全交接

**Files:**
- Create: `crates/desktop-core/src/vapid_key.rs`
- Create: `crates/desktop-core/src/secret_file.rs`
- Modify: `crates/desktop-core/src/named_tunnel.rs`
- Modify: `crates/desktop-core/src/bridge_process.rs`
- Modify: `crates/desktop-core/src/lib.rs`
- Modify: `Cargo.toml`
- Modify: `crates/desktop-core/Cargo.toml`
- Modify: `crates/bridge-core/Cargo.toml`
- Modify: `apps/desktop-shell/src-tauri/src/main.rs`
- Create: `crates/bridge-core/src/vapid.rs`
- Modify: `crates/bridge-core/src/lib.rs`
- Modify: `apps/bridge-sidecar/src/main.rs`
- Test: `crates/desktop-core/src/vapid_key.rs`
- Test: `crates/desktop-core/src/secret_file.rs`
- Test: `crates/bridge-core/src/vapid.rs`

- [ ] **Step 1: 增加 P-256 依赖并写 key 稳定性测试**

workspace dependencies 增加：

```toml
p256 = "0.13"
```

`desktop-core` 增加 `base64`、`p256`；`bridge-core` 增加 `p256`（`base64` 已存在）。先写测试：

```rust
#[test]
fn key_manager_generates_once_and_reuses_keychain_value() {
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
    let manager = VapidKeyManager::new(Arc::clone(&secrets));

    let first = manager.load_or_create().unwrap();
    let second = manager.load_or_create().unwrap();

    assert_eq!(first, second);
    assert_eq!(first.private_key_base64.len(), 43);
    assert_eq!(URL_SAFE_NO_PAD.decode(&first.public_key_base64).unwrap().len(), 65);
    assert_ne!(first.private_key_base64, first.public_key_base64);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run:

```bash
cargo test -p desktop-core key_manager_generates_once_and_reuses_keychain_value -- --exact
```

Expected: FAIL，因为 `VapidKeyManager` 尚不存在。

- [ ] **Step 3: 实现 VAPID key manager**

```rust
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::{SecretKey, elliptic_curve::{rand_core::OsRng, sec1::ToEncodedPoint}};

#[derive(Debug, Error)]
pub enum VapidKeyError {
    #[error("VAPID Keychain operation failed: {0}")]
    SecretStore(#[from] SecretStoreError),
    #[error("stored VAPID private key is invalid")]
    InvalidStoredKey,
}

#[derive(Clone, PartialEq, Eq)]
pub struct VapidKeyMaterial {
    pub private_key_base64: String,
    pub public_key_base64: String,
}

impl std::fmt::Debug for VapidKeyMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("VapidKeyMaterial")
            .field("public_key", &"[available]")
            .field("private_key", &"[REDACTED]")
            .finish()
    }
}

pub struct VapidKeyManager {
    secrets: Arc<dyn SecretStore>,
}

impl VapidKeyManager {
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self { Self { secrets } }

    pub fn load_or_create(&self) -> Result<VapidKeyMaterial, VapidKeyError> {
        let private_key_base64 = match self.secrets.get(VAPID_PRIVATE_KEY_KEY)? {
            Some(value) => value,
            None => {
                let key = SecretKey::random(&mut OsRng);
                let value = URL_SAFE_NO_PAD.encode(key.to_bytes());
                self.secrets.set(VAPID_PRIVATE_KEY_KEY, &value)?;
                value
            }
        };
        let key_bytes = URL_SAFE_NO_PAD.decode(&private_key_base64)
            .map_err(|_| VapidKeyError::InvalidStoredKey)?;
        let key = SecretKey::from_slice(&key_bytes)
            .map_err(|_| VapidKeyError::InvalidStoredKey)?;
        let public_key_base64 = URL_SAFE_NO_PAD.encode(
            key.public_key().to_encoded_point(false).as_bytes(),
        );
        Ok(VapidKeyMaterial { private_key_base64, public_key_base64 })
    }
}
```

错误信息不能包含 key 内容。扩展 Step 1 测试，断言 `format!("{first:?}")` 不包含 `first.private_key_base64`。

- [ ] **Step 4: 把短期 secret file 提取成通用模块**

把 Phase 1 `TemporarySecretFile` 移到 `secret_file.rs`，公开为：

```rust
pub struct TemporarySecretFile {
    path: PathBuf,
}

#[derive(Debug, Error)]
pub enum SecretFileError {
    #[error("temporary secret file I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

impl TemporarySecretFile {
    pub fn create(
        runtime_dir: &Path,
        filename_prefix: &str,
        secret: &[u8],
    ) -> Result<Self, SecretFileError>;
    pub fn path(&self) -> &Path;
    pub fn remove(self) -> Result<(), SecretFileError>;
}
```

Unix mode 必须 `0600`，Drop best-effort 删除。`named_tunnel.rs` 改为：

```rust
TemporarySecretFile::create(&runtime_dir, "cloudflared-token", token.as_bytes())?
```

运行原 Named Tunnel 权限/清理测试确保无回归。

- [ ] **Step 5: 写 sidecar secret 消费测试**

`bridge-core/src/vapid.rs` 测试：

```rust
#[test]
fn reads_vapid_secret_file_once_and_removes_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vapid-secret");
    std::fs::write(&path, TEST_PRIVATE_KEY_BASE64).unwrap();

    let material = VapidRuntimeKey::from_secret_file(&path).unwrap();

    assert!(!path.exists());
    assert_eq!(material.private_key_base64(), TEST_PRIVATE_KEY_BASE64);
    assert_eq!(material.public_key_bytes().len(), 65);
}
```

- [ ] **Step 6: 实现 sidecar runtime key**

`VapidRuntimeKey` 在构造时验证 32-byte private key，派生 public key；只实现自定义 `Debug`：

```rust
#[derive(Debug, Error)]
pub enum VapidRuntimeKeyError {
    #[error("VAPID secret file I/O failed")]
    Io,
    #[error("VAPID private key is invalid")]
    InvalidKey,
}

pub struct VapidRuntimeKey {
    private_key_base64: String,
    public_key_base64: String,
    public_key_bytes: Vec<u8>,
}

impl std::fmt::Debug for VapidRuntimeKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("VapidRuntimeKey")
            .field("public_key", &"[available]")
            .field("private_key", &"[REDACTED]")
            .finish()
    }
}
```

并提供只读访问器：

```rust
impl VapidRuntimeKey {
    pub fn from_secret_file(path: &Path) -> Result<Self, VapidRuntimeKeyError>;
    pub fn private_key_base64(&self) -> &str;
    pub fn public_key_base64(&self) -> &str;
    pub fn public_key_bytes(&self) -> &[u8];
}
```

`from_secret_file` 用 `std::fs::read_to_string`，无论解析成功或失败都尝试删除文件。桌面层类型固定叫 `VapidKeyMaterial`，sidecar/bridge 层固定叫 `VapidRuntimeKey`，不要在跨 crate 代码中把两个名称混用。

- [ ] **Step 7: BridgeProcessConfig 支持额外 env**

新增：

```rust
pub extra_env: Vec<(String, String)>,
```

默认空；`prepare_launch_plan` 在内置 env 后 extend。测试只允许路径：

```rust
assert_eq!(
    plan.env.iter()
        .find(|(key, _)| key == "CODEX_MOBILE_BRIDGE_VAPID_KEY_FILE")
        .map(|(_, value)| value.as_str()),
    Some("/tmp/vapid-secret")
);
assert!(!format!("{plan:?}").contains(TEST_PRIVATE_KEY_BASE64));
```

- [ ] **Step 8: Tauri 在每一次 Bridge manager 构造前写 VAPID secret file**

`ShellState` 增加 `vapid_keys: VapidKeyManager` 和 `pending_vapid_secret: Mutex<Option<TemporarySecretFile>>`，两者复用 Phase 1 的同一个 `Arc<dyn SecretStore>`。把“配置端口策略 + 注入 VAPID file + 构造 manager”集中到唯一的 async helper；初次启动、Phase 1 `ensure_bridge_for_mode` 的 Fixed/Flexible 重建、Bridge 重启都必须调用它，不能只修改某一个 startup command：

```rust
async fn build_bridge_manager_for_mode(
    app: &AppHandle,
    state: &ShellState,
    mode: BridgePortMode,
) -> Result<BridgeProcessManager, String> {
    let mut config = bridge_config(app)?;
    match mode {
        BridgePortMode::Flexible => config.port_policy = PortPolicy::Flexible,
        BridgePortMode::Fixed(port) => {
            config.preferred_port = Some(port);
            config.port_policy = PortPolicy::Fixed;
        }
    }

    let app_data_dir = config.app_data_dir.clone();
    let vapid = state.vapid_keys.load_or_create()
        .map_err(|error| error.to_string())?;
    let secret_file = TemporarySecretFile::create(
        &app_data_dir.join("runtime"),
        "vapid-key",
        vapid.private_key_base64.as_bytes(),
    ).map_err(|error| error.to_string())?;
    config.extra_env.push((
        "CODEX_MOBILE_BRIDGE_VAPID_KEY_FILE".to_string(),
        secret_file.path().display().to_string(),
    ));

    *state.pending_vapid_secret.lock().await = Some(secret_file);
    Ok(BridgeProcessManager::new(config))
}
```

Phase 1 `ensure_bridge_for_mode` 不再直接调用 `BridgeProcessManager::new(config)`，而是调用该 helper。`manager.start().await` 返回 health Ready 后 `take()` 并 drop/remove pending file；spawn/health 失败路径同样 `take()` 清理。若构造新 manager 前仍有旧 pending file，先 drop 旧值。路径可进入 env，私钥不得进入 env/args。

- [ ] **Step 9: Sidecar 读取 env file**

```rust
let vapid_key: Option<Arc<VapidRuntimeKey>> = env::var_os("CODEX_MOBILE_BRIDGE_VAPID_KEY_FILE")
    .map(PathBuf::from)
    .map(VapidRuntimeKey::from_secret_file)
    .transpose()
    .context("load VAPID runtime key")?
    .map(Arc::new);
```

没有 env 时 Bridge 正常启动，但 push capability 为 unavailable，方便直接运行 sidecar 开发。这个 `Option<Arc<VapidRuntimeKey>>` 保持到 sidecar 进程退出，Task 2 注入 `AppState`，Task 3 同一个 Arc 再交给 sender；不得重新读取 secret file。

- [ ] **Step 10: 运行测试并提交**

Run:

```bash
cargo test -p desktop-core vapid
cargo test -p desktop-core secret_file
cargo test -p desktop-core named_tunnel
cargo test -p bridge-core vapid
cargo test -p desktop-shell
```

Expected: PASS；测试输出、Debug 和 env 均无 private key。

```bash
git add Cargo.toml Cargo.lock crates/desktop-core/Cargo.toml crates/bridge-core/Cargo.toml crates/desktop-core/src/vapid_key.rs crates/desktop-core/src/secret_file.rs crates/desktop-core/src/named_tunnel.rs crates/desktop-core/src/bridge_process.rs crates/desktop-core/src/lib.rs apps/desktop-shell/src-tauri/src/main.rs crates/bridge-core/src/vapid.rs crates/bridge-core/src/lib.rs apps/bridge-sidecar/src/main.rs
git commit -m "feat: provision VAPID keys through Keychain"
```

## Task 2: PushSubscription、Public Key 与设备撤销

**Files:**
- Modify: `crates/bridge-core/src/notification_store.rs`
- Modify: `crates/bridge-core/src/http_api.rs`
- Modify: `crates/bridge-core/src/protocol.rs`
- Modify: `apps/bridge-sidecar/src/main.rs`
- Modify: `apps/mobile-pwa/src/notifications/api.ts`
- Modify: `apps/mobile-pwa/src/notifications/api.test.ts`
- Test: `crates/bridge-core/src/notification_store.rs`
- Test: `crates/bridge-core/src/http_api.rs`

- [ ] **Step 1: 写 subscription replace/invalid/revoke 测试**

```rust
#[test]
fn subscription_is_replaced_per_device_and_can_be_invalidated() {
    let (_dir, store) = test_store();
    store.save_subscription(&subscription("phone-1", "https://push/one")).unwrap();
    store.save_subscription(&subscription("phone-1", "https://push/two")).unwrap();

    let current = store.active_subscription("phone-1").unwrap().unwrap();
    assert_eq!(current.endpoint, "https://push/two");

    store.invalidate_subscription("phone-1", 20).unwrap();
    assert_eq!(store.active_subscription("phone-1").unwrap(), None);
}

#[test]
fn deleting_device_notification_data_removes_settings_subscription_and_deliveries() {
    let (_dir, store) = test_store();
    seed_all_notification_rows(&store, "phone-1");

    store.delete_device_notification_data("phone-1").unwrap();

    assert!(store.settings_row("phone-1").unwrap().is_none());
    assert!(store.subscription_row("phone-1").unwrap().is_none());
    assert_eq!(store.delivery_count("phone-1").unwrap(), 0);
}
```

这是对 Phase 2 已有 `delete_device_notification_data` 的扩展：保留 settings 删除，并在同一个 transaction 中增加 subscription 和 delivery 删除。

- [ ] **Step 2: 运行 store 测试确认失败**

Run:

```bash
cargo test -p bridge-core subscription_is_replaced_per_device_and_can_be_invalidated -- --exact
```

Expected: FAIL，因为 schema 和方法尚不存在。

- [ ] **Step 3: 增加 subscription 与 delivery schema**

```sql
CREATE TABLE IF NOT EXISTS push_subscriptions (
    device_id TEXT PRIMARY KEY NOT NULL,
    origin TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    p256dh TEXT NOT NULL,
    auth TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_success_at INTEGER,
    invalidated_at INTEGER
);

CREATE TABLE IF NOT EXISTS notification_deliveries (
    event_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    status TEXT NOT NULL,
    attempt_count INTEGER NOT NULL,
    next_attempt_at INTEGER NOT NULL,
    last_error_category TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (event_id, device_id)
);
```

模型：

```rust
pub struct PushSubscriptionRecord {
    pub device_id: String,
    pub origin: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub created_at: u64,
    pub last_success_at: Option<u64>,
    pub invalidated_at: Option<u64>,
}
```

任何 `Debug` 实现只显示 endpoint host 和 keys `[REDACTED]`。

- [ ] **Step 4: 写 public key 模式测试**

```rust
#[tokio::test]
async fn public_key_is_only_available_for_named_https_mode() {
    let app = authenticated_app_with_vapid();

    set_access(&app, "quick", "https://temp.trycloudflare.com").await;
    assert_api_error(
        get_public_key_from(&app, "https://temp.trycloudflare.com").await,
        StatusCode::CONFLICT,
        "push_unavailable",
    );

    set_access(&app, "named", "https://codex.example.com").await;
    let response = get_public_key_from(&app, "https://codex.example.com").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["publicKey"], TEST_PUBLIC_KEY_BASE64);

    assert_api_error(
        get_public_key_from(&app, "http://192.168.1.10:57324").await,
        StatusCode::CONFLICT,
        "push_unavailable",
    );
}
```

给 `ApiErrorCode` 增加：

```rust
PushUnavailable,
InvalidSubscription,
```

TypeScript shared protocol同步对应 snake_case 值。

- [ ] **Step 5: 写 subscription origin 与字段校验测试**

覆盖：

- body origin、当前请求 effective origin 与当前 Named public origin 三者完全一致 => `201`；
- origin 为 Quick 或不匹配 => `409 push_unavailable`；
- endpoint 非 HTTPS => `400 invalid_subscription`；
- p256dh/auth 不是 URL-safe base64 或解码长度异常 => `400`；
- 同一 device 二次注册覆盖；另一个 device 不受影响；
- DELETE 仅删除当前 authenticated device。

- [ ] **Step 6: 实现三个 push API**

先把 runtime key 注入 HTTP state：

```rust
// AppState
vapid_key: Option<Arc<VapidRuntimeKey>>,

pub fn with_vapid_key(mut self, vapid_key: Arc<VapidRuntimeKey>) -> Self {
    self.vapid_key = Some(vapid_key);
    self
}
```

`AppState::new` 默认 `None`。sidecar 在 Task 1 得到 `vapid_key` 后使用 `Arc::clone` 注入 state；测试 helper `authenticated_app_with_vapid` 也必须走这个 builder。`GET /api/push/public-key` 只返回 `public_key_base64()`，任何 API、DTO 或 `Debug` 都不得暴露 `private_key_base64()`。

authenticated routes：

```rust
.route("/api/push/public-key", get(get_push_public_key))
.route("/api/push/subscription", post(save_push_subscription).delete(delete_push_subscription))
```

request：

```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PushSubscriptionRequest {
    origin: String,
    endpoint: String,
    keys: PushSubscriptionKeysRequest,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PushSubscriptionKeysRequest {
    p256dh: String,
    auth: String,
}
```

限制：endpoint <= 4096 chars；p256dh/auth <= 512；endpoint 必须是带 host 的 HTTPS URL，不允许 username、password 或 fragment（push service 自身的 path/query 保留）；p256dh decode 65 bytes；auth decode 16 bytes。request body `origin`、Phase 2 的 `effective_request_origin(headers)` 和 `PublicAccessState` 当前 Named origin 必须三者相等；LAN 入口即使全局 Named 正常也返回 `push_unavailable`。

- [ ] **Step 7: Settings 返回真实 subscription state**

规则：

- 非 Named 或无 VAPID => `unavailable`
- permission 由前端报告，服务端无 active record => `not_enabled`
- active record 且 `record.origin == current named origin` => `active`
- active record 但 origin 不匹配 => `needs_repair`
- invalidated record => `needs_repair`

`deliveryMode` 只在“当前请求来自 Named Origin + VAPID 可用”时改为 `web_push`，`systemNotifications=true`；LAN/Quick 继续 `foreground_only`。

- [ ] **Step 8: 设备撤销同步清理**

Phase 2 的 `revoke_device` 已调用该清理方法；本阶段保持同一调用点，并验证扩展后的 transaction 同时删除 subscription/outbox：

```rust
state.notification_store
    .lock().await
    .delete_device_notification_data(&device_id)?;
```

若 notification cleanup 失败，返回 500 并记录类别；不要恢复已完成的 revoke。重复 revoke/cleanup 保持幂等。

- [ ] **Step 9: PWA API client 严格解析**

增加：

```ts
export async function getPushPublicKey(session: DeviceSession): Promise<string>;
export async function savePushSubscription(
  session: DeviceSession,
  origin: string,
  subscription: PushSubscriptionJSON,
): Promise<void>;
export async function deletePushSubscription(session: DeviceSession): Promise<void>;
```

`PushSubscriptionJSON.keys.p256dh/auth` 缺失时客户端先报 `Invalid PushSubscription`，不发送请求。

- [ ] **Step 10: 运行测试并提交**

Run:

```bash
cargo test -p bridge-core notification_store
cargo test -p bridge-core push_public_key
cargo test -p bridge-core push_subscription
cargo test -p bridge-core revoke_device
cd apps/mobile-pwa && npm test -- --run src/notifications/api.test.ts src/bridge-protocol.test.ts
```

Expected: PASS。

```bash
git add crates/bridge-core/src/notification_store.rs crates/bridge-core/src/http_api.rs crates/bridge-core/src/protocol.rs packages/bridge-protocol/src/protocol.ts apps/bridge-sidecar/src/main.rs apps/mobile-pwa/src/notifications/api.ts apps/mobile-pwa/src/notifications/api.test.ts apps/mobile-pwa/src/bridge-protocol.test.ts
git commit -m "feat: manage authenticated push subscriptions"
```

## Task 3: Web Push Sender、Outbox 与有限重试

**Files:**
- Create: `crates/bridge-core/src/web_push.rs`
- Create: `crates/bridge-core/src/push_delivery_worker.rs`
- Modify: `Cargo.toml`
- Modify: `crates/bridge-core/Cargo.toml`
- Modify: `crates/bridge-core/src/lib.rs`
- Modify: `crates/bridge-core/src/notification_store.rs`
- Modify: `crates/bridge-core/src/notification_dispatcher.rs`
- Modify: `crates/bridge-core/src/http_api.rs`
- Modify: `apps/bridge-sidecar/src/main.rs`
- Test: `crates/bridge-core/src/web_push.rs`
- Test: `crates/bridge-core/src/push_delivery_worker.rs`

- [ ] **Step 1: 增加 web-push 依赖并写 payload 隐私测试**

workspace：

```toml
web-push = "0.11.0"
```

`bridge-core` 增加依赖。测试：

```rust
#[test]
fn push_payload_contains_only_allowed_alert_fields_and_delivery_hints() {
    let payload = PushPayload::for_event(
        &alert(AlertKind::Error),
        DeliveryHints {
            sound_enabled: true,
            vibration_enabled: true,
            force_system_notification: false,
        },
    );
    let value = serde_json::to_value(payload).unwrap();

    assert_eq!(
        value.as_object().unwrap().keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "eventId".into(), "kind".into(), "threadId".into(),
            "threadTitle".into(), "occurredAt".into(),
            "vibrationEnabled".into(), "vibrationPattern".into(),
            "silent".into(), "forceSystemNotification".into(),
        ])
    );
    assert!(!value.to_string().contains("cwd"));
    assert!(!value.to_string().contains("reply"));
}
```

- [ ] **Step 2: 实现 payload 和 transport trait**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushPayload {
    pub event_id: String,
    pub kind: AlertKind,
    pub thread_id: String,
    pub thread_title: String,
    pub occurred_at: u64,
    pub vibration_enabled: bool,
    pub vibration_pattern: Vec<u16>,
    pub silent: bool,
    pub force_system_notification: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct DeliveryHints {
    pub sound_enabled: bool,
    pub vibration_enabled: bool,
    pub force_system_notification: bool,
}

impl PushPayload {
    pub fn for_event(event: &AlertEvent, hints: DeliveryHints) -> Self {
        let vibration_pattern = if hints.vibration_enabled {
            match event.kind {
                AlertKind::Completed => vec![80],
                AlertKind::ApprovalRequired => vec![80, 60, 80],
                AlertKind::InputRequired => vec![45, 40, 45],
                AlertKind::Error => vec![150, 80, 150],
            }
        } else {
            Vec::new()
        };
        Self {
            event_id: event.event_id.clone(),
            kind: event.kind,
            thread_id: event.thread_id.clone(),
            thread_title: event.thread_title.clone(),
            occurred_at: event.occurred_at,
            vibration_enabled: hints.vibration_enabled,
            vibration_pattern,
            silent: !hints.sound_enabled,
            force_system_notification: hints.force_system_notification,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushFailureClass {
    InvalidSubscription,
    Retryable,
    Permanent,
}

impl PushFailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSubscription => "push_invalid_subscription",
            Self::Retryable => "push_retryable",
            Self::Permanent => "push_permanent",
        }
    }
}

#[derive(Debug, Error)]
pub enum WebPushTransportError {
    #[error("push endpoint returned HTTP {0}")]
    HttpStatus(u16),
    #[error("push request timed out")]
    Timeout,
    #[error("push network request failed")]
    Network,
    #[error("push subscription material is invalid")]
    InvalidSubscriptionMaterial,
    #[error("VAPID key material is invalid")]
    InvalidVapidKey,
    #[error("push payload is too large")]
    PayloadTooLarge,
}

#[async_trait]
pub trait WebPushTransport: Send + Sync {
    async fn send(
        &self,
        subscription: &PushSubscriptionRecord,
        payload: &[u8],
        vapid_private_key_base64: &str,
    ) -> Result<(), WebPushTransportError>;
}

#[derive(Clone)]
pub struct WebPushSender {
    transport: Arc<dyn WebPushTransport>,
    vapid_key: Arc<VapidRuntimeKey>,
}

impl WebPushSender {
    pub fn new(
        transport: Arc<dyn WebPushTransport>,
        vapid_key: Arc<VapidRuntimeKey>,
    ) -> Self {
        Self { transport, vapid_key }
    }

    pub async fn send(
        &self,
        subscription: &PushSubscriptionRecord,
        payload: &PushPayload,
    ) -> Result<(), PushFailureClass> {
        let bytes = serde_json::to_vec(payload)
            .map_err(|_| PushFailureClass::Permanent)?;
        self.transport
            .send(
                subscription,
                &bytes,
                self.vapid_key.private_key_base64(),
            )
            .await
            .map_err(classify_web_push_error)
    }
}
```

`RustWebPushTransport` 使用 `SubscriptionInfo`、`VapidSignatureBuilder::from_base64`、`WebPushMessageBuilder`、`ContentEncoding::Aes128Gcm`、TTL 300 秒和 `IsahcWebPushClient`。外层 `tokio::time::timeout(Duration::from_secs(10), send)`，因为底层 client 自身不超时。

- [ ] **Step 3: 写错误分类测试**

```rust
#[test]
fn classifies_push_errors_for_retry_and_invalidation() {
    assert_eq!(classify_web_push_error(test_error(410)), PushFailureClass::InvalidSubscription);
    assert_eq!(classify_web_push_error(test_error(404)), PushFailureClass::InvalidSubscription);
    assert_eq!(classify_web_push_error(test_error(429)), PushFailureClass::Retryable);
    assert_eq!(classify_web_push_error(test_error(503)), PushFailureClass::Retryable);
    assert_eq!(classify_web_push_error(test_error(400)), PushFailureClass::Permanent);
    assert_eq!(classify_web_push_error(WebPushTransportError::Timeout), PushFailureClass::Retryable);
}
```

- [ ] **Step 4: 实现错误映射**

把 web-push error 映射为不含 endpoint/body 的内部错误：

- `EndpointNotFound`/`EndpointNotValid` 或 code 404/410 => InvalidSubscription
- timeout、network unspecified、408、429、5xx => Retryable
- invalid keys、400/401/403、payload too large => Permanent

普通日志仅输出 `push_invalid_subscription`、`push_retryable_5xx` 等类别。

`classify_web_push_error(error: WebPushTransportError) -> PushFailureClass` 必须穷举上述 enum，不解析可能包含 endpoint 或 response body 的自由文本。

- [ ] **Step 5: 写 outbox 唯一约束和重启恢复测试**

```rust
#[test]
fn enqueue_is_idempotent_per_event_and_device() {
    let (_dir, store) = test_store();
    assert!(store.enqueue_delivery(&delivery("event-1", "phone-1")).unwrap());
    assert!(!store.enqueue_delivery(&delivery("event-1", "phone-1")).unwrap());
    assert_eq!(store.delivery_count("phone-1").unwrap(), 1);
}

#[test]
fn reopening_store_recovers_sending_rows_to_pending() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bridge.sqlite");
    let store = NotificationStore::open(&path).unwrap();
    store.enqueue_delivery(&delivery("event-1", "phone-1")).unwrap();
    let claimed = store.claim_next_due_delivery(10).unwrap().unwrap();
    assert_eq!(claimed.status, DeliveryStatus::Sending);
    assert_eq!(claimed.attempt_count, 1);
    drop(store);

    let reopened = NotificationStore::open(&path).unwrap();
    let recovered = reopened.claim_next_due_delivery(20).unwrap().unwrap();
    assert_eq!(recovered.status, DeliveryStatus::Sending);
    assert_eq!(recovered.attempt_count, 2);
}
```

- [ ] **Step 6: 实现 outbox 状态转换**

`NotificationStore::open` migration 后执行：

```sql
UPDATE notification_deliveries
SET status = 'pending'
WHERE status = 'sending';
```

公开模型和状态固定为：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    Pending,
    Sending,
    Sent,
    InvalidSubscription,
    Failed,
}

#[derive(Debug, Clone)]
pub struct NotificationDelivery {
    pub event_id: String,
    pub device_id: String,
    pub payload_json: String,
    pub status: DeliveryStatus,
    pub attempt_count: u32,
    pub next_attempt_at: u64,
    pub last_error_category: Option<String>,
    pub updated_at: u64,
}
```

公开方法：

```rust
pub fn enqueue_delivery(&self, delivery: &NotificationDelivery) -> Result<bool>;
pub fn claim_next_due_delivery(&self, now: u64) -> Result<Option<NotificationDelivery>>;
pub fn next_delivery_due_at(&self) -> Result<Option<u64>>;
pub fn mark_delivery_sent(&self, event_id: &str, device_id: &str, now: u64) -> Result<()>;
pub fn mark_delivery_retry(
    &self,
    event_id: &str,
    device_id: &str,
    next_attempt_at: u64,
    category: &str,
    now: u64,
) -> Result<()>;
pub fn mark_delivery_invalid_subscription(
    &self,
    event_id: &str,
    device_id: &str,
    now: u64,
) -> Result<()>;
pub fn mark_delivery_failed(
    &self,
    event_id: &str,
    device_id: &str,
    category: &str,
    now: u64,
) -> Result<()>;
pub fn fail_pending_deliveries(&self, category: &str, now: u64) -> Result<usize>;
```

`claim_next_due_delivery` 必须在一个 SQLite transaction 中选择 due row、把状态改为 `sending`、递增 `attempt_count` 并返回更新后的 row，避免多个 worker 重复 claim。`attempt_count` 表示已经实际发起的发送次数。初次失败后最多重试 3 次，延迟依次为 1s/2s/4s；第 4 次发送仍失败后进入 terminal `failed`。新增测试断言持续 503 时 transport 总调用 4 次且之后不再进入 due queue。

`mark_delivery_sent` 必须在同一个 transaction 中把 delivery 改为 `sent`，并更新当前 active `push_subscriptions.last_success_at = now`。增加成功发送测试，断言 delivery 为 `Sent` 且对应 subscription 的 `last_success_at` 已更新；否则 Phase 3 诊断页声明的 `lastSuccessAt` 永远不会产生真实数据。

- [ ] **Step 7: 写 worker 多设备隔离测试**

```rust
#[tokio::test]
async fn one_invalid_subscription_does_not_block_other_devices() {
    let transport = Arc::new(ScriptedTransport::new([
        ("phone-a", Err(WebPushTransportError::HttpStatus(410))),
        ("phone-b", Ok(())),
    ]));
    let harness = WorkerHarness::new(transport);
    harness.enqueue_same_event_for(["phone-a", "phone-b"]).await;

    harness.worker.drain_due_once().await.unwrap();

    assert_eq!(harness.delivery_status("phone-a"), DeliveryStatus::InvalidSubscription);
    assert_eq!(harness.delivery_status("phone-b"), DeliveryStatus::Sent);
    assert!(harness.subscription("phone-a").is_none());
    assert!(harness.subscription("phone-b").is_some());
}
```

- [ ] **Step 8: 实现 PushDeliveryWorker**

Worker 循环：claim due row -> 获取 active subscription -> deserialize payload -> send -> 分类更新。claim 和读取 subscription 后必须释放 store mutex，再执行网络 send；响应回来后重新加锁写状态，不能持有 SQLite mutex 等待公网。payload 反序列化失败时把该 delivery 标记为 terminal `failed`，类别固定为 `invalid_push_payload`；active subscription 缺失或已 invalidated 时标记为 `InvalidSubscription`，不能让 row 永久停在 `sending`。每次处理后继续下一设备；单设备错误不 return 整个 drain。

公开结构和构造函数固定为：

```rust
pub struct PushDeliveryWorker {
    store: Arc<Mutex<NotificationStore>>,
    sender: WebPushSender,
    public_access: PublicAccessState,
    wake: Arc<Notify>,
}

impl PushDeliveryWorker {
    pub fn new(
        store: Arc<Mutex<NotificationStore>>,
        sender: WebPushSender,
        public_access: PublicAccessState,
        wake: Arc<Notify>,
    ) -> Self {
        Self { store, sender, public_access, wake }
    }
}
```

`delivery.payload_json` 必须先严格反序列化为 `PushPayload`，再传给 sender：

```rust
match self.sender.send(&subscription, &payload).await {
    Ok(()) => store.mark_delivery_sent(
        &delivery.event_id,
        &delivery.device_id,
        now,
    )?,
    Err(PushFailureClass::InvalidSubscription) => {
        store.invalidate_subscription(&delivery.device_id, now)?;
        store.mark_delivery_invalid_subscription(
            &delivery.event_id,
            &delivery.device_id,
            now,
        )?;
    }
    Err(PushFailureClass::Retryable) if delivery.attempt_count <= 3 => {
        let retry_index = (delivery.attempt_count - 1) as usize;
        let next_attempt_at = now + [1_000, 2_000, 4_000][retry_index];
        store.mark_delivery_retry(
            &delivery.event_id,
            &delivery.device_id,
            next_attempt_at,
            PushFailureClass::Retryable.as_str(),
            now,
        )?;
    }
    Err(class) => store.mark_delivery_failed(
        &delivery.event_id,
        &delivery.device_id,
        class.as_str(),
        now,
    )?,
}
```

Worker 使用 `Notify`，enqueue 后立即 wake；无 due row 时最多 sleep 30 秒，并按 nearest `next_attempt_at` 提前唤醒。

每次 claim 前读取 `public_access.current()`：只有 mode 为 Named 才处理 outbox。取到 subscription、释放 store mutex 后，在实际调用 sender 前再次读取一次最新 `public_access.current()`；只有最新状态仍为 Named 且 `subscription.origin == latest.public_origin` 才能发送。不匹配时 invalidate subscription 并把该 delivery 标记 `InvalidSubscription`，不向旧 Origin 的 subscription 发送。第一次读取只用于避免无效 claim，第二次读取才是发送前的竞态保护。

- [ ] **Step 9: Dispatcher 同时 WS + enqueue push**

在 Phase 2 的 `NotificationDispatcher` 上增加可选 runtime，不改变无 Push 时的构造方式：

```rust
// NotificationDispatcher
push_runtime: Option<PushDispatchRuntime>,

#[derive(Clone)]
pub struct PushDispatchRuntime {
    public_access: PublicAccessState,
    wake: Arc<Notify>,
}

pub fn with_push_runtime(
    mut self,
    public_access: PublicAccessState,
    wake: Arc<Notify>,
) -> Self {
    self.push_runtime = Some(PushDispatchRuntime { public_access, wake });
    self
}
```

Phase 2 的 `NotificationDispatcher::new(store, event_hub)` 将 `push_runtime` 初始化为 `None`。

sidecar 只在 `vapid_key.is_some()` 时创建一个共享 `Arc<Notify>`，同一个实例同时传给 Dispatcher 和 `PushDeliveryWorker`。对每个 settings target：

1. 始终 `publish_to_device` 前台 envelope；
2. 仅当 `PublicAccessMode::Named`、VAPID 可用、active subscription 存在且 `subscription.origin == current public origin` 时 enqueue；
3. payload hints 来自该设备 settings：`sound_enabled` 映射为 `silent = !sound_enabled`，vibration 使用对应全局开关；
4. Quick/local 绝不 enqueue。

判断模式时调用 `runtime.public_access.current().await`，先取得 context，再获取 store mutex；不得在持有 store mutex 时等待 `PublicAccessState` 的 RwLock 或执行 WebSocket/网络操作。

同一个 event 对同一 device 重复 dispatch 时 SQLite unique 约束返回 false，不产生第二条 push；只有 `enqueue_delivery` 返回 true 时调用 `wake.notify_one()`。普通事件使用 `dispatch_to_device(device_id, event, false)` 同时走 WS + eligible push。`dispatch_test_to_device` 单独处理：若当前 Named 且 subscription 可用，只 enqueue `forceSystemNotification=true` 的 push，不发送 WS，避免一次测试同时播放前台声音和系统通知；Quick/local/push unavailable 时才发送 WS 前台测试。测试仍只针对当前 authenticated device，并允许 master 关闭时验证链路。

Phase 2 的 test 方法签名在本阶段改为可传播 outbox 错误：

```rust
pub async fn dispatch_test_to_device(
    &self,
    device_id: &str,
    event: AlertEvent,
) -> anyhow::Result<usize>;
```

`POST /api/notifications/test` handler 对该结果使用 `?` 映射为固定 `internal_error`，不能在 enqueue 失败时仍返回 `202`。

- [ ] **Step 10: 测试提醒固定模式走真实 outbox**

`POST /api/notifications/test`：Named + active subscription 模式只 enqueue `forceSystemNotification=true`；Quick/local 或 push unavailable 模式只发 WS。测试事件只发送当前 authenticated device。新增测试让 Settings 页面保持 visible，断言固定模式 WebSocket 不收到该 test event，而 outbox 恰有一条 force payload。

扩展 `/api/control/remote-access`：从 Named 切到 Quick/Local，或 Named hostname 发生变化时，在更新 context 的同一个 handler 中调用 `fail_pending_deliveries("public_access_changed", now)`；不删除 subscription，以便回到同一固定 Origin 时继续使用，但旧 pending/retry 不会在恢复后迟到发送。新增测试先 enqueue retry row，再切 Quick，断言 row terminal failed 且 transport 未调用。

- [ ] **Step 11: Sidecar 启动 delivery worker**

用 Task 1 保持在内存中的同一个 `Arc<VapidRuntimeKey>` 构造 real transport、`WebPushSender` 和 worker，并把 Phase 2 的同一个 `PublicAccessState` clone 传给 worker，`tokio::spawn(worker.run())`。没有 VAPID key 时不配置 dispatcher push runtime、不启动 worker，Settings capability 为 unavailable，Bridge 其他功能正常。

- [ ] **Step 12: 运行测试并提交**

Run:

```bash
cargo test -p bridge-core web_push
cargo test -p bridge-core push_delivery_worker
cargo test -p bridge-core notification_dispatcher
cargo test -p bridge-core notifications_test
```

Expected: PASS；任何失败输出不包含 endpoint path、p256dh、auth 或 VAPID private key。

```bash
git add Cargo.toml Cargo.lock crates/bridge-core/Cargo.toml crates/bridge-core/src/web_push.rs crates/bridge-core/src/push_delivery_worker.rs crates/bridge-core/src/lib.rs crates/bridge-core/src/notification_store.rs crates/bridge-core/src/notification_dispatcher.rs crates/bridge-core/src/http_api.rs apps/bridge-sidecar/src/main.rs
git commit -m "feat: deliver alerts through direct web push"
```

## Task 4: 构建 Service Worker、IndexedDB 去重和通知点击

**Files:**
- Create: `apps/mobile-pwa/src/notifications/push-protocol.ts`
- Create: `apps/mobile-pwa/src/notifications/push-protocol.test.ts`
- Create: `apps/mobile-pwa/src/notifications/recent-event-store.ts`
- Create: `apps/mobile-pwa/src/notifications/recent-event-store.test.ts`
- Create: `apps/mobile-pwa/src/notifications/service-worker-handlers.ts`
- Create: `apps/mobile-pwa/src/notifications/service-worker-handlers.test.ts`
- Create: `apps/mobile-pwa/src/service-worker.ts`
- Modify: `apps/mobile-pwa/src/main.tsx`
- Modify: `apps/mobile-pwa/vite.config.ts`
- Modify: `apps/mobile-pwa/tsconfig.json`
- Create: `apps/mobile-pwa/tsconfig.sw.json`
- Modify: `apps/mobile-pwa/package.json`
- Modify: `apps/mobile-pwa/package-lock.json`
- Delete: `apps/mobile-pwa/public/sw.js`

- [ ] **Step 1: 增加 fake-indexeddb 并写 payload parser 测试**

在 devDependencies 增加：

```json
"fake-indexeddb": "^6.0.0"
```

测试：

```ts
it("accepts_the_minimal_private_alert_payload_and_rejects_extra_sensitive_fields", () => {
  const valid = {
    eventId: "alert-1",
    kind: "completed",
    threadId: "thread-1",
    threadTitle: "Release",
    occurredAt: 1784349000000,
    vibrationEnabled: true,
    vibrationPattern: [80],
    silent: false,
    forceSystemNotification: false,
  };
  expect(parsePushPayload(valid)).toEqual(valid);
  expect(parsePushPayload({ ...valid, cwd: "/secret/project" })).toBeNull();
  expect(parsePushPayload({ ...valid, reply: "private response" })).toBeNull();
});

it("maps_all_four_kinds_to_the_approved_system_notification_copy", () => {
  expect(notificationCopy("completed").body).toBe("Codex task completed");
  expect(notificationCopy("approval_required").body).toBe("Codex is waiting for approval");
  expect(notificationCopy("input_required").body).toBe("Codex needs more input");
  expect(notificationCopy("error").body).toBe("Codex task stopped with an error");
});
```

- [ ] **Step 2: 实现严格 PushPayload parser**

`push-protocol.ts` 明确允许字段集合；object 出现任何未知字段即返回 null。`threadTitle` 最大 200 chars，IDs 非空且最大 256 chars，vibration pattern 最多 7 个非负整数、每项 <= 1000。

公开类型和 client message guard：

```ts
export interface PushPayload extends AlertEvent {
  vibrationEnabled: boolean;
  vibrationPattern: number[];
  silent: boolean;
  forceSystemNotification: boolean;
}

export type AlertClientMessage = {
  type: "codex_alert_event";
  payload: AlertEvent;
};

export type OpenThreadMessage = {
  type: "open_thread";
  threadId: string;
};

export function isAlertClientMessage(value: unknown): value is AlertClientMessage;
export function isOpenThreadMessage(value: unknown): value is OpenThreadMessage;

export function notificationCopy(kind: AlertKind): { body: string } {
  return {
    completed: { body: "Codex task completed" },
    approval_required: { body: "Codex is waiting for approval" },
    input_required: { body: "Codex needs more input" },
    error: { body: "Codex task stopped with an error" },
  }[kind];
}
```

导出：

```ts
export function alertEventFromPush(payload: PushPayload): AlertEvent {
  return {
    eventId: payload.eventId,
    kind: payload.kind,
    threadId: payload.threadId,
    threadTitle: payload.threadTitle,
    occurredAt: payload.occurredAt,
  };
}
```

- [ ] **Step 3: 写 IndexedDB 原子 claim 测试**

```ts
import "fake-indexeddb/auto";

it("claims_an_event_once_and_allows_it_after_ttl_expiry", async () => {
  const store = new RecentEventStore({ ttlMs: 1000, maxEntries: 2 });

  expect(await store.claim("event-1", 100)).toBe(true);
  expect(await store.claim("event-1", 200)).toBe(false);
  expect(await store.claim("event-1", 1200)).toBe(true);
});

it("prunes_oldest_rows_when_capacity_is_exceeded", async () => {
  const store = new RecentEventStore({ ttlMs: 10_000, maxEntries: 2 });
  await store.claim("event-1", 1);
  await store.claim("event-2", 2);
  await store.claim("event-3", 3);
  expect(await store.has("event-1", 4)).toBe(false);
});
```

- [ ] **Step 4: 实现 RecentEventStore**

数据库：`codex-mobile-notifications-v1`；object store `recent_alert_events`，keyPath `eventId`，index `expiresAt`。`claim` 在一个 readwrite transaction 中 get + put，随后删除 expired rows，并按 `occurredAt` 删除超出 256 的最旧记录。

默认 TTL 7 天：

```ts
const DEFAULT_TTL_MS = 7 * 24 * 60 * 60 * 1000;
const DEFAULT_MAX_ENTRIES = 256;
```

公开 API 固定为：

```ts
export class RecentEventStore {
  constructor(options?: { ttlMs?: number; maxEntries?: number });
  claim(eventId: string, now?: number): Promise<boolean>;
  has(eventId: string, now?: number): Promise<boolean>;
}
```

IndexedDB 失败时返回 `true` 允许当前提醒继续，不能因去重存储故障吞掉通知；同时只 console.warn 固定错误类别。

- [ ] **Step 5: 写 visible/locked/force 分支测试**

```ts
it("posts_to_visible_clients_without_showing_a_system_notification", async () => {
  const env = serviceWorkerHarness({ visibleClients: 1 });
  await handlePush(validPayload(), env);
  expect(env.postedMessages).toHaveLength(1);
  expect(env.notifications).toHaveLength(0);
});

it("shows_system_notification_with_the_device_sound_setting_when_no_visible_client_exists", async () => {
  const env = serviceWorkerHarness({ visibleClients: 0 });
  await handlePush(validPayload({ kind: "error", silent: true }), env);
  expect(env.notifications[0]).toMatchObject({
    title: "Release",
    options: {
      body: "Codex task stopped with an error",
      tag: "alert-1",
      silent: true,
    },
  });
});

it("force_system_notification_shows_even_while_settings_page_is_visible", async () => {
  const env = serviceWorkerHarness({ visibleClients: 1 });
  await handlePush(validPayload({ forceSystemNotification: true }), env);
  expect(env.notifications).toHaveLength(1);
  expect(env.postedMessages).toHaveLength(0);
});
```

- [ ] **Step 6: 实现可测试 handler**

`service-worker-handlers.ts` 接收接口：

```ts
export interface ServiceWorkerNotificationOptions extends NotificationOptions {
  vibrate?: number[];
}

export interface WindowClientPort {
  readonly visibilityState: "hidden" | "visible" | "prerender";
  readonly url: string;
  postMessage(message: AlertClientMessage | OpenThreadMessage): void;
  focus(): Promise<WindowClientPort>;
}

export interface ServiceWorkerEnvironment {
  claimEvent(eventId: string, now: number): Promise<boolean>;
  visibleWindowClients(): Promise<WindowClientPort[]>;
  showNotification(title: string, options: ServiceWorkerNotificationOptions): Promise<void>;
  openWindow(url: string): Promise<WindowClientPort | null>;
  allWindowClients(): Promise<WindowClientPort[]>;
}
```

当前 TypeScript `lib.webworker.d.ts` 的 `NotificationOptions` 没有声明已被浏览器实现的 `vibrate` 字段，因此测试端和纯 handler 使用上面的扩展类型；实际 environment adapter 调用 `worker.registration.showNotification(title, options as NotificationOptions)`。这个窄 cast 只能放在浏览器边界，payload parser 和 handler 内部仍保持严格类型，避免 `tsconfig.sw.json` 因 excess property 编译失败。

`handlePush` 先 parse，再 claim；duplicate 直接 return。visible + 非 force 时 post：

```ts
{ type: "codex_alert_event", payload: alertEventFromPush(payload) }
```

后台 notification options：

```ts
{
  body,
  tag: payload.eventId,
  icon: "/icon-192.png",
  badge: "/icon-192.png",
  data: { threadId: payload.threadId, eventId: payload.eventId },
  silent: payload.silent,
  vibrate: payload.vibrationEnabled ? payload.vibrationPattern : undefined,
}
```

`handleNotificationClick` 构造新窗口 URL 时必须使用 `encodeURIComponent(threadId)`；现有 client focus 成功后再 post `open_thread`，focus/openWindow 失败只记录固定类别。

- [ ] **Step 7: 写 notification click 测试**

```ts
it("focuses_an_existing_client_and_requests_the_target_thread", async () => {
  const env = serviceWorkerHarness({ allClients: 1 });
  await handleNotificationClick({ threadId: "thread-9" }, env);
  expect(env.focusedClients).toEqual([0]);
  expect(env.postedMessages[0]).toEqual({ type: "open_thread", threadId: "thread-9" });
});

it("opens_a_new_window_with_thread_query_when_no_client_exists", async () => {
  const env = serviceWorkerHarness({ allClients: 0 });
  await handleNotificationClick({ threadId: "thread-9" }, env);
  expect(env.openedUrls).toEqual(["/?threadId=thread-9"]);
});
```

- [ ] **Step 8: 实现实际 Service Worker**

`service-worker.ts` 由独立 `tsconfig.sw.json` 使用 WebWorker lib 类型检查，绑定 install/activate/fetch/push/notificationclick。保留现有 API/WebSocket network-only 规则和 shell cache，把 cache name 升为 `codex-mobile-shell-v2`；install 完成缓存后调用 `skipWaiting()`，activate 删除旧 cache 并调用 `clients.claim()`。

push event：

```ts
worker.addEventListener("push", (event) => {
  event.waitUntil((async () => {
    let raw: unknown;
    try {
      raw = event.data?.json();
    } catch {
      console.warn("invalid_push_payload");
      return;
    }
    await handlePush(raw, environment);
  })());
});
```

文件顶部使用：

```ts
const worker = globalThis as unknown as ServiceWorkerGlobalScope;
```

JSON parse 异常只记录 `invalid_push_payload`，不显示空通知。

- [ ] **Step 9: 配置 Vite multi-entry build**

先拆分页面与 worker 的 TypeScript globals。`tsconfig.json` 增加：

```json
"exclude": ["src/service-worker.ts"]
```

新建 `tsconfig.sw.json`：

```json
{
  "extends": "./tsconfig.json",
  "compilerOptions": {
    "lib": ["ES2022", "WebWorker"],
    "types": ["vite/client"],
    "noEmit": true
  },
  "include": [
    "src/service-worker.ts",
    "src/notifications/push-protocol.ts",
    "src/notifications/recent-event-store.ts",
    "src/notifications/service-worker-handlers.ts",
    "../../packages/bridge-protocol/src"
  ],
  "exclude": ["src/**/*.test.ts", "src/**/*.test.tsx"]
}
```

`package.json` 的 build 改为：

```json
"build": "tsc --noEmit && tsc -p tsconfig.sw.json --noEmit && vite build"
```

`vite.config.ts` 是 ESM，不能使用 `__dirname`。使用：

```ts
import { fileURLToPath } from "node:url";

const appEntry = fileURLToPath(new URL("./index.html", import.meta.url));
const serviceWorkerEntry = fileURLToPath(
  new URL("./src/service-worker.ts", import.meta.url),
);

build: {
  rollupOptions: {
    input: {
      app: appEntry,
      sw: serviceWorkerEntry,
    },
    output: {
      entryFileNames: (chunk) =>
        chunk.name === "sw" ? "sw.js" : "assets/[name]-[hash].js",
      chunkFileNames: "assets/[name]-[hash].js",
      assetFileNames: "assets/[name]-[hash][extname]",
    },
  },
},
```

`main.tsx` 注册：

```ts
navigator.serviceWorker.register("/sw.js", { type: "module" });
```

删除 `public/sw.js`，避免 public copy 覆盖构建产物。构建后验证 `dist/sw.js` 存在且不包含 VAPID private key。

- [ ] **Step 10: 运行测试/build 并提交**

Run:

```bash
cd apps/mobile-pwa && npm install
npm test -- --run src/notifications/push-protocol.test.ts src/notifications/recent-event-store.test.ts src/notifications/service-worker-handlers.test.ts
npm run build
test -f dist/sw.js
```

Expected: PASS。

```bash
git add apps/mobile-pwa/src/notifications/push-protocol.ts apps/mobile-pwa/src/notifications/push-protocol.test.ts apps/mobile-pwa/src/notifications/recent-event-store.ts apps/mobile-pwa/src/notifications/recent-event-store.test.ts apps/mobile-pwa/src/notifications/service-worker-handlers.ts apps/mobile-pwa/src/notifications/service-worker-handlers.test.ts apps/mobile-pwa/src/service-worker.ts apps/mobile-pwa/src/main.tsx apps/mobile-pwa/vite.config.ts apps/mobile-pwa/tsconfig.json apps/mobile-pwa/tsconfig.sw.json apps/mobile-pwa/package.json apps/mobile-pwa/package-lock.json apps/mobile-pwa/public/sw.js
git commit -m "feat: handle push notifications in service worker"
```

## Task 5: PushSubscription Controller、iPhone 安装门槛和 Settings 集成

**Files:**
- Create: `apps/mobile-pwa/src/notifications/push-subscription-controller.ts`
- Create: `apps/mobile-pwa/src/notifications/push-subscription-controller.test.ts`
- Modify: `apps/mobile-pwa/src/notifications/capabilities.ts`
- Modify: `apps/mobile-pwa/src/notifications/capabilities.test.ts`
- Modify: `apps/mobile-pwa/src/notifications/NotificationSettingsPage.tsx`
- Modify: `apps/mobile-pwa/src/notifications/NotificationSettingsPage.test.tsx`
- Modify: `apps/mobile-pwa/src/notifications/NotificationOnboardingSheet.tsx`
- Modify: `apps/mobile-pwa/src/notifications/NotificationOnboardingSheet.test.tsx`
- Modify: `apps/mobile-pwa/src/App.tsx`
- Modify: `apps/mobile-pwa/src/App.test.tsx`
- Modify: `apps/mobile-pwa/src/styles.css`

- [ ] **Step 1: 写临时模式绝不请求权限测试**

```ts
it("does_not_request_permission_or_subscribe_in_foreground_only_mode", async () => {
  const ports = pushPorts({ permission: "default" });
  const controller = new PushSubscriptionController(ports);

  await expect(controller.enable(capabilities({ fixedHttps: false })))
    .rejects.toMatchObject({ code: "push_unavailable" });

  expect(ports.requestPermission).not.toHaveBeenCalled();
  expect(ports.subscribe).not.toHaveBeenCalled();
});
```

- [ ] **Step 2: 写 iPhone standalone 和 denied 测试**

```ts
it("requires_home_screen_install_on_ios_before_permission_prompt", async () => {
  const ports = pushPorts({ permission: "default" });
  const controller = new PushSubscriptionController(ports);

  await expect(controller.enable(capabilities({ isIos: true, standalone: false })))
    .rejects.toMatchObject({ code: "ios_install_required" });
  expect(ports.requestPermission).not.toHaveBeenCalled();
});

it("does_not_reprompt_when_notification_permission_is_denied", async () => {
  const ports = pushPorts({ permission: "denied" });
  const controller = new PushSubscriptionController(ports);

  await expect(controller.enable(fixedAndroid())).rejects.toMatchObject({ code: "permission_denied" });
  expect(ports.requestPermission).not.toHaveBeenCalled();
});
```

- [ ] **Step 3: 写完整 enable/repair/disable 测试**

```ts
it("requests_permission_subscribes_and_registers_with_the_bridge", async () => {
  const ports = pushPorts({ permission: "default", requestedPermission: "granted" });
  const controller = new PushSubscriptionController(ports);

  await controller.enable(fixedAndroid());

  expect(ports.getPublicKey).toHaveBeenCalled();
  expect(ports.subscribe).toHaveBeenCalledWith({
    userVisibleOnly: true,
    applicationServerKey: expect.any(Uint8Array),
  });
  expect(ports.saveSubscription).toHaveBeenCalledWith(expect.objectContaining({
    origin: "https://codex.example.com",
  }));
});

it("repair_unsubscribes_stale_browser_record_before_resubscribing", async () => {
  const order: string[] = [];
  const stale = fakeSubscription("stale", async () => { order.push("unsubscribe-browser"); });
  const fresh = fakeSubscription("fresh");
  const ports = pushPorts({ permission: "granted" });
  ports.deleteServerSubscription.mockImplementation(async () => { order.push("delete-server"); });
  ports.getSubscription
    .mockImplementationOnce(async () => stale)
    .mockImplementationOnce(async () => null);
  ports.subscribe.mockImplementation(async () => {
    order.push("subscribe-browser");
    return fresh;
  });
  ports.saveSubscription.mockImplementation(async () => { order.push("save-server"); });
  const controller = new PushSubscriptionController(ports);

  await controller.repair(fixedAndroid());

  expect(order).toEqual([
    "delete-server",
    "unsubscribe-browser",
    "subscribe-browser",
    "save-server",
  ]);
});

it("disable_attempts_server_and_browser_cleanup_when_one_side_fails", async () => {
  const stale = fakeSubscription("stale");
  const ports = pushPorts({ permission: "granted", existing: stale });
  ports.deleteServerSubscription.mockRejectedValue(new Error("bridge unavailable"));
  const controller = new PushSubscriptionController(ports);

  await expect(controller.disable()).rejects.toMatchObject({ code: "disable_failed" });

  expect(ports.deleteServerSubscription).toHaveBeenCalledTimes(1);
  expect(stale.unsubscribe).toHaveBeenCalledTimes(1);
});
```

- [ ] **Step 4: 实现 capability detection**

```ts
export interface PushCapabilities {
  fixedHttps: boolean;
  secureContext: boolean;
  serviceWorker: boolean;
  pushManager: boolean;
  notificationApi: boolean;
  isIos: boolean;
  standalone: boolean;
}
```

`isIos` 可使用 iPhone/iPad platform hint；最终是否可订阅仍以 ServiceWorker/PushManager/Notification feature detection 为准。standalone 检查 `matchMedia("(display-mode: standalone)").matches` 或 iOS `navigator.standalone === true`。

读取 iOS 专有属性时使用显式局部类型，不能直接访问标准 `Navigator` 上不存在的字段：

```ts
type NavigatorWithStandalone = Navigator & { standalone?: boolean };
const iosStandalone = (navigator as NavigatorWithStandalone).standalone === true;
```

- [ ] **Step 5: 实现 PushSubscriptionController**

把浏览器和 API 副作用放入可测试 ports：

```ts
export interface BrowserPushSubscription {
  toJSON(): PushSubscriptionJSON;
  unsubscribe(): Promise<boolean>;
}

export interface PushSubscriptionPorts {
  permission(): NotificationPermission;
  requestPermission(): Promise<NotificationPermission>;
  getPublicKey(): Promise<string>;
  getSubscription(): Promise<BrowserPushSubscription | null>;
  subscribe(options: PushSubscriptionOptionsInit): Promise<BrowserPushSubscription>;
  saveSubscription(input: {
    origin: string;
    subscription: PushSubscriptionJSON;
  }): Promise<void>;
  deleteServerSubscription(): Promise<void>;
  origin(): string;
}
```

测试文件实现 `capabilities(overrides)`、`fakeSubscription(id, onUnsubscribe)` 和 `pushPorts(options)` fixture；`pushPorts` 的每个方法都是 `vi.fn`，默认 fixed HTTPS、permission granted、无现有 subscription，避免测试直接修改浏览器全局对象。

启用顺序固定：

```ts
if (!capabilities.fixedHttps) throw pushError("push_unavailable");
if (capabilities.isIos && !capabilities.standalone) throw pushError("ios_install_required");
if (!capabilities.secureContext || !capabilities.serviceWorker || !capabilities.pushManager || !capabilities.notificationApi) {
  throw pushError("unsupported");
}
if (this.ports.permission() === "denied") throw pushError("permission_denied");
const permission = this.ports.permission() === "granted"
  ? "granted"
  : await this.ports.requestPermission();
if (permission !== "granted") throw pushError("permission_denied");
const publicKey = await this.ports.getPublicKey();
const existing = await this.ports.getSubscription();
const subscription = existing ?? await this.ports.subscribe({
  userVisibleOnly: true,
  applicationServerKey: urlBase64ToUint8Array(publicKey),
});
await this.ports.saveSubscription({
  origin: this.ports.origin(),
  subscription: subscription.toJSON(),
});
```

`repair` 固定按 server delete -> browser unsubscribe -> `enable` 执行；`disable` 用两个独立 try/catch 确保 server delete 与 browser unsubscribe 都被尝试，最后若任一失败抛 `{ code: "disable_failed" }`，错误对象不得包含 endpoint 或 keys。

- [ ] **Step 6: Settings 展示真实系统状态**

状态映射：

- Active: permission granted + browser subscription + server active
- Not enabled: default/no subscription
- Blocked: permission denied
- Needs repair: granted 但 browser/server 任一缺失或服务端 invalidated
- Unavailable: Quick/local/unsupported

Vibration 在 iPhone 显示“由系统控制”并禁用前台 vibration 开关；Android 支持时允许开关。系统声音说明“后台使用系统通知声音”，不能提供自定义锁屏 sound selector。Sound 关闭时 payload 设置 `silent=true`，在支持的平台请求静音；iPhone 仍明确标注最终声音由系统控制，不能承诺 Web Push 的 `silent` hint 一定覆盖系统设置。

按钮：`Enable system notifications`、`Repair notifications`、`Disable alerts`、`Send test alert`，按状态只显示可执行命令。`Disable alerts` 先 PUT 完整 settings 把 master 设为 false，再调用 controller disable；即使 subscription cleanup 失败，服务端 master 已关闭也不能再 enqueue 新提醒，UI 显示 cleanup/repair 错误而不是重新打开 master。

- [ ] **Step 7: 首次引导接入真实 push enable**

用户点击 Enable：先 `player.unlock()`；固定模式再调用 controller enable；成功后 PUT master/all kinds/sound/vibration true；最后发送 force system test。iPhone 非 standalone 显示安装步骤，不请求 permission，也不把 onboarding 标记完成；用户 Not now 才 dismiss。

Quick 模式点击 Enable 只启用前台提醒并 dismiss，页面明确 `Lock-screen alerts require a fixed HTTPS address configured on the Mac`。

- [ ] **Step 8: App 接收 Service Worker message 并共用 eventId 去重**

```ts
useEffect(() => {
  function handleServiceWorkerMessage(event: MessageEvent) {
    if (isAlertClientMessage(event.data)) {
      void alertPlayerRef.current.handle(event.data.payload, notificationSettingsRef.current);
    }
    if (isOpenThreadMessage(event.data)) {
      setPendingNotificationThreadId(event.data.threadId);
    }
  }
  navigator.serviceWorker?.addEventListener("message", handleServiceWorkerMessage);
  return () => navigator.serviceWorker?.removeEventListener("message", handleServiceWorkerMessage);
}, []);
```

WS 和 SW 都调用同一个 `ForegroundAlertPlayer` instance；同 eventId 只响一次。

- [ ] **Step 9: 实现 notification deep-link**

初次 load 读取 `threadId` query，保存到 pending state，不在 sessions 未加载时丢弃。session list 到达后：存在则选择并切回 workbench；不存在则保留当前 session 并显示 `Session is no longer available`。处理后用 `history.replaceState` 删除 `threadId` 参数。

若设备 session 已撤销，现有 auth flow 进入 Unpaired/Connection error，不能在未鉴权时展示 session 内容。

- [ ] **Step 10: 运行 PWA 测试/build 并提交**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run src/notifications/push-subscription-controller.test.ts src/notifications/capabilities.test.ts src/notifications/NotificationSettingsPage.test.tsx src/notifications/NotificationOnboardingSheet.test.tsx src/App.test.tsx
npm run build
```

Expected: PASS。

```bash
git add apps/mobile-pwa/src/notifications/push-subscription-controller.ts apps/mobile-pwa/src/notifications/push-subscription-controller.test.ts apps/mobile-pwa/src/notifications/capabilities.ts apps/mobile-pwa/src/notifications/capabilities.test.ts apps/mobile-pwa/src/notifications/NotificationSettingsPage.tsx apps/mobile-pwa/src/notifications/NotificationSettingsPage.test.tsx apps/mobile-pwa/src/notifications/NotificationOnboardingSheet.tsx apps/mobile-pwa/src/notifications/NotificationOnboardingSheet.test.tsx apps/mobile-pwa/src/App.tsx apps/mobile-pwa/src/App.test.tsx apps/mobile-pwa/src/styles.css
git commit -m "feat: enable lock-screen push notifications"
```

## Task 6: 安全诊断、发布门禁、真机 QA 与 v0.1.7

**Files:**
- Modify: `crates/desktop-core/src/diagnostics_bundle.rs`
- Modify: `apps/desktop-shell/src-tauri/src/main.rs`
- Modify: `scripts/check-release-gate.sh`
- Modify: `docs/release-gates.md`
- Modify: `docs/dogfood-qa-checklist.md`
- Create: `docs/qa/2026-07-18-web-push-device-matrix.md`
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

- [x] **Step 1: 写 push secret 脱敏测试**

```rust
#[test]
fn redacts_vapid_and_subscription_material() {
    let input = concat!(
        "CODEX_MOBILE_BRIDGE_VAPID_KEY_FILE=/Users/damon/vapid-secret\n",
        "VAPID_PRIVATE_KEY=private-base64-value\n",
        "p256dh=public-client-key auth=client-auth-secret\n",
        "endpoint=https://fcm.googleapis.com/fcm/send/private-path"
    );
    let redacted = redact_sensitive_text(input);

    for secret in ["private-base64-value", "public-client-key", "client-auth-secret", "private-path"] {
        assert!(!redacted.contains(secret));
    }
    assert!(redacted.contains("fcm.googleapis.com"));
}
```

- [x] **Step 2: 实现 endpoint host-only 诊断**

`redact_sensitive_text` 增加 VAPID/private/subscription markers。不要只依靠通用 token 规则；显式处理 `VAPID_PRIVATE_KEY`、`p256dh`、`auth`、`endpoint`。诊断 DTO 只返回：

```json
{
  "subscriptionState": "active",
  "endpointHost": "fcm.googleapis.com",
  "lastSuccessAt": 1784349000000,
  "lastErrorCategory": null
}
```

不得返回 endpoint path/query、keys、payload JSON 或 VAPID key。

- [x] **Step 3: 加强 stable release gate**

当 `ENABLE_WEB_PUSH=true` 且 channel stable：

```bash
[ "${STABLE_REMOTE_ACCESS_PROVIDER:-}" = "named_tunnel" ] \
  || fail "stable Web Push requires STABLE_REMOTE_ACCESS_PROVIDER=named_tunnel"
[ "${PUSH_QA_IOS_ACK:-false}" = "true" ] \
  || fail "stable Web Push requires PUSH_QA_IOS_ACK=true"
[ "${PUSH_QA_ANDROID_ACK:-false}" = "true" ] \
  || fail "stable Web Push requires PUSH_QA_ANDROID_ACK=true"
```

新增 shell 测试或直接在计划执行时验证缺失/满足变量两种分支。更新 `docs/release-gates.md` 的变量说明。

- [x] **Step 4: 创建真机 QA 矩阵**

`docs/qa/2026-07-18-web-push-device-matrix.md` 使用表格记录：设备/OS/浏览器或 PWA/Origin/四类通知/点击 deep-link/声音/震动/结果/日期。至少要求：

- iPhone 当前支持版本，固定域名，添加主屏幕后四类锁屏通知；
- Android Chrome 浏览器页与安装 PWA；
- permission denied、系统撤销、Repair；
- Bridge 重启、Mac 换 Wi-Fi、Named Tunnel 短断网；
- 404/410 subscription 失效；
- 同一事件 WS + Push 同时抵达只提示一次；
- 设备撤销后不再收到通知；
- Quick Tunnel 不创建 PushSubscription。

- [x] **Step 5: 更新 dogfood 清单**

```markdown
- [ ] iPhone 已添加到主屏幕后才能请求通知权限；Safari 普通标签页不误报可用。
- [ ] 固定域名锁屏后四类状态在 15 秒内收到系统通知。
- [ ] 前台可见时普通 push 不弹系统通知，只转发页面并按 eventId 去重。
- [ ] Send test alert 即使 Settings 可见也弹一条系统通知。
- [ ] Android 支持时震动模式不同；iPhone 标明由系统控制。
- [ ] permission denied 不重复弹权限请求；Repair 可恢复丢失 subscription。
- [ ] 404/410 将 subscription 标记 needs repair，不持续重试。
- [ ] 408/429/5xx 初次失败后最多重试 3 次（总发送最多 4 次），某设备失败不阻塞其他设备。
- [ ] 从 Named 手动切到 Quick/Local 后，pending/retry outbox 被终止，不会在临时模式或恢复后补发旧提醒。
- [ ] Bridge 重启恢复 pending outbox，不重复生成 AlertEvent。
```

- [x] **Step 6: 统一升级版本到 0.1.7**

更新 `VERSION`、内部 Cargo/package/Tauri 版本和 package-lock 顶层版本。

Run:

```bash
scripts/check-version-sync.sh
```

Expected: `Version 0.1.7 is synchronized across desktop, sidecar, and PWA manifests.`

- [ ] **Step 7: 运行完整自动回归和安全扫描**

Run:

```bash
cargo fmt --all -- --check
cargo test --workspace
cd apps/mobile-pwa && npm test -- --run && npm run build
cd ../desktop-shell && npm test -- --run && npm run build
cd ../.. && scripts/check-release-gate.sh --channel dev
rg -n "VAPID_PRIVATE_KEY|private-base64-value|p256dh=|auth=|--token [^[]" target apps crates -g '*.log' -g '*.json' -g '*.txt'
```

Expected: tests/build PASS；最后一个 `rg` 不命中真实 secret，只允许测试 fixture 中的固定假值。

- [ ] **Step 8: 构建 DMG 并完成 LAN/Named 真机 QA**

Run:

```bash
cd apps/desktop-shell && npm run tauri:build
```

Expected: `0.1.7` DMG。安装后使用固定域名重新配对并重新授权；完成 QA matrix。Quick Tunnel 只验证前台提醒与限制提示，不测试锁屏 push。

- [ ] **Step 9: 验证 stable gate 的 Web Push 分支**

Run without acknowledgements:

```bash
ENABLE_WEB_PUSH=true STABLE_REMOTE_ACCESS_PROVIDER=named_tunnel scripts/check-release-gate.sh --channel stable
```

Expected: FAIL，明确缺少 iOS/Android QA ack（签名变量可能更早失败时，使用现有测试 harness 对 gate helper 做隔离测试）。

Run in CI/test harness with全部签名、公证、updater 和 QA fake env：Expected stable gate PASS。

- [ ] **Step 10: 提交 Phase 3**

```bash
git add crates/desktop-core/src/diagnostics_bundle.rs apps/desktop-shell/src-tauri/src/main.rs scripts/check-release-gate.sh docs/release-gates.md docs/dogfood-qa-checklist.md docs/qa/2026-07-18-web-push-device-matrix.md VERSION crates/bridge-core/Cargo.toml crates/desktop-core/Cargo.toml apps/bridge-sidecar/Cargo.toml apps/desktop-shell/src-tauri/Cargo.toml apps/desktop-shell/src-tauri/tauri.conf.json apps/desktop-shell/package.json apps/desktop-shell/package-lock.json apps/mobile-pwa/package.json apps/mobile-pwa/package-lock.json
git commit -m "release: prepare web push beta v0.1.7"
```

## Phase 3 验收门槛

- VAPID private key 只存在 Keychain、sidecar 内存和一次性 `0600` 文件；文件读取后立即删除；args/env value/log/diagnostics/API 不含私钥。
- Public key 和 subscription API 只在 Named HTTPS 模式可用；Quick Tunnel 永远不创建或复用 PushSubscription。
- 离开 Named Origin 时 pending/retry outbox 立即终止；worker 每次发送前复核当前 Named Origin 与 subscription origin。
- subscription 绑定 authenticated device；替换、删除、404/410 invalidation 和设备撤销均有隔离测试。
- delivery 使用 `(event_id, device_id)` 唯一约束；重启恢复 sending rows；网络错误最多重试 3 次（总发送最多 4 次）；单设备失败不阻塞其他设备。
- push payload 不含正文、错误详情、CWD、工具参数或 secret。
- 可见页面收到 push 时不显示系统通知，只 postMessage；后台/锁屏显示系统通知；force test 始终显示。
- WS 与 SW 共用前台 player 和 eventId 去重；notification click 聚焦/打开 PWA 并选择 thread。
- iPhone 非 standalone 不请求 permission；denied 不反复请求；granted 但 subscription 丢失可 Repair。
- iPhone 与 Android 真机 QA matrix 完成；固定域名锁屏提醒 15 秒内到达；平台不支持项有准确降级文案。
- 全部 Rust/PWA/desktop tests、build、版本同步、release gate 和 secret 扫描通过。
