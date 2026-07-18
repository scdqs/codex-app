# 固定域名 Named Tunnel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 Mac Bridge 增加可持久化的 Cloudflare Named Tunnel、固定本地端口、Keychain Token、端到端验证和显式手动 Quick Tunnel 降级。

**Architecture:** 保留现有 `QuickTunnelManager`，新增独立的 `NamedTunnelManager` 和桌面层远程访问协调逻辑；两种公网入口不能同时运行。非敏感配置写入应用数据目录，Tunnel Token 只存 macOS Keychain，并通过权限为 `0600` 的短期 `--token-file` 交给 bundled `cloudflared`。

**Tech Stack:** Rust 2024、Tokio、Reqwest、SQLite、keyring 3.6、Tauri 2、TypeScript、Vitest、Cloudflare `cloudflared 2026.7.1`。

---

## 文件结构

- Create `crates/desktop-core/src/secret_store.rs`: Keychain 抽象、macOS keyring 实现和测试用内存实现。
- Create `crates/desktop-core/src/remote_access_config.rs`: Named Tunnel 非敏感配置、hostname/port 校验和原子持久化。
- Create `crates/desktop-core/src/named_tunnel.rs`: Named Tunnel 进程、短期 Token 文件、端到端 health 验证、有限重试和失败分类。
- Modify `crates/desktop-core/src/bridge_process.rs`: 增加固定端口策略和每次 Bridge 启动唯一的 instance ID。
- Modify `crates/desktop-core/src/diagnostics_bundle.rs`: 脱敏 Tunnel Token、token-file 路径和远程访问诊断字段。
- Modify `crates/desktop-core/src/lib.rs`, `crates/desktop-core/Cargo.toml`, `Cargo.toml`: 导出新模块并增加依赖。
- Modify `crates/bridge-core/src/http_api.rs`, `crates/bridge-core/src/storage.rs`, `crates/bridge-core/src/pairing.rs`, `packages/bridge-protocol/src/status.ts`: health 返回 `instanceId`，设备记录配对 Origin。
- Modify `apps/bridge-sidecar/src/main.rs`: 从环境接收 Bridge instance ID。
- Modify `apps/mobile-pwa/src/api.ts`, `apps/mobile-pwa/src/api.test.ts`: 配对时提交当前 Origin。
- Modify `apps/desktop-shell/src-tauri/src/main.rs`, `apps/desktop-shell/src-tauri/Cargo.toml`: 初始化远程访问服务，增加 Named Tunnel Tauri commands，协调 Bridge/Quick/Named 生命周期。
- Create `apps/desktop-shell/src/remote-access.ts`, `apps/desktop-shell/src/remote-access.test.ts`: 三步向导的类型、纯状态转换和 HTML 渲染。
- Modify `apps/desktop-shell/src/main.ts`, `apps/desktop-shell/src/styles.css`, `apps/desktop-shell/package.json`: 接入三步向导、失败操作和桌面测试命令。
- Modify `VERSION` 及所有版本清单：完成 Phase 1 后统一升至 `0.1.5`。

## Task 1: 固定端口策略与 Bridge 实例身份

**Files:**
- Modify: `crates/desktop-core/src/bridge_process.rs:39-125,163-229,385-474,510-538`
- Modify: `crates/bridge-core/src/http_api.rs:46-59,106-112,243-274,421-427`
- Modify: `apps/bridge-sidecar/src/main.rs:25-83`
- Modify: `packages/bridge-protocol/src/status.ts:1-5`
- Test: `crates/desktop-core/src/bridge_process.rs`
- Test: `crates/bridge-core/src/http_api.rs`
- Test: `apps/mobile-pwa/src/bridge-protocol.test.ts`

- [ ] **Step 1: 先写固定端口冲突测试**

在 `bridge_process.rs` 测试模块中把现有随机回退测试保留给 `Flexible`，并新增：

```rust
#[test]
fn launch_plan_rejects_an_occupied_fixed_port() {
    let temp = TempDir::new().expect("temp dir creates");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("test listener binds");
    let occupied_port = listener.local_addr().expect("listener addr").port();
    let mut config = test_config(&temp);
    config.preferred_port = Some(occupied_port);
    config.port_policy = PortPolicy::Fixed;
    let manager = BridgeProcessManager::new(config);

    let error = manager
        .prepare_launch_plan()
        .expect_err("fixed occupied port must fail");

    assert!(matches!(
        error,
        BridgeProcessError::PreferredPortUnavailable { port } if port == occupied_port
    ));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run:

```bash
cargo test -p desktop-core launch_plan_rejects_an_occupied_fixed_port -- --exact
```

Expected: FAIL，因为 `PortPolicy` 和 `PreferredPortUnavailable` 尚不存在。

- [ ] **Step 3: 实现显式端口策略**

在 `bridge_process.rs` 增加：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortPolicy {
    Flexible,
    Fixed,
}
```

给 `BridgeProcessConfig` 增加：

```rust
pub port_policy: PortPolicy,
```

给 `BridgeProcessSnapshot` 同样增加只读字段，供桌面协调层判断当前 manager 是否真的按固定端口策略创建：

```rust
pub port_policy: PortPolicy,
```

`BridgeProcessManager::snapshot` 必须从 `self.config.port_policy` 填充该字段。新增测试断言默认 manager 的 snapshot 为 `Flexible`，固定配置 manager 的 snapshot 为 `Fixed`；不能只比较当前端口号，因为 Flexible manager 也可能碰巧占用与 Named profile 相同的端口。

默认值保持兼容：

```rust
port_policy: PortPolicy::Flexible,
```

给 `BridgeProcessError` 增加：

```rust
#[error("preferred bridge port {port} is unavailable")]
PreferredPortUnavailable { port: u16 },
```

把端口选择改为：

```rust
fn choose_port(
    bind_ip: IpAddr,
    preferred_port: Option<u16>,
    policy: PortPolicy,
) -> Result<u16, BridgeProcessError> {
    if let Some(port) = preferred_port {
        if TcpListener::bind(SocketAddr::new(bind_ip, port)).is_ok() {
            return Ok(port);
        }
        if policy == PortPolicy::Fixed {
            return Err(BridgeProcessError::PreferredPortUnavailable { port });
        }
    }

    let listener = TcpListener::bind(SocketAddr::new(bind_ip, 0)).map_err(|source| {
        BridgeProcessError::Io {
            path: PathBuf::from(format!("{bind_ip}:0")),
            source,
        }
    })?;
    Ok(listener
        .local_addr()
        .map_err(|source| BridgeProcessError::Io {
            path: PathBuf::from(format!("{bind_ip}:0")),
            source,
        })?
        .port())
}
```

并把调用改为：

```rust
let port = choose_port(
    self.config.bind_ip,
    self.config.preferred_port,
    self.config.port_policy,
)?;
```

- [ ] **Step 4: 写 health instance ID 失败测试**

在 `http_api.rs` 的 health 测试中增加断言：

```rust
assert_eq!(payload["instanceId"], json!("bridge-instance-test"));
assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
```

并让测试 state 使用：

```rust
let state = test_state().with_instance_id("bridge-instance-test");
```

- [ ] **Step 5: 运行 health 测试确认失败**

Run:

```bash
cargo test -p bridge-core health_route_reports_bridge_status -- --exact
```

Expected: FAIL，因为 health payload 还没有 `instanceId`。

- [ ] **Step 6: 实现 Bridge instance ID**

给 `AppState` 增加字段和 builder：

```rust
instance_id: Arc<str>,

pub fn with_instance_id(mut self, instance_id: impl Into<Arc<str>>) -> Self {
    self.instance_id = instance_id.into();
    self
}
```

`AppState::new` 默认生成 UUID，避免改动所有测试调用方：

```rust
instance_id: Arc::<str>::from(Uuid::new_v4().to_string()),
```

扩展 health DTO：

```rust
pub struct HealthResponse {
    status: String,
    connection_state: String,
    version: &'static str,
    instance_id: Arc<str>,
}
```

health handler 返回：

```rust
instance_id: Arc::clone(&state.instance_id),
```

health response 增加 `Cache-Control: no-store`。Named Tunnel 的本地/公网 probe 同时发送 `Cache-Control: no-cache`，避免用户自定义 Cloudflare cache rule 返回旧 Bridge 的 health identity，导致错误的 Ready 或 WrongBridgeInstance 判断。

`BridgeLaunchPlan` 增加：

```rust
pub instance_id: String,
```

`prepare_launch_plan` 每次生成新的实例 ID，并把同一个值同时放入 plan 和 launch env。不要把 instance ID 存在 `BridgeProcessManager` 字段上，否则同一个 manager stop/start 后会错误复用旧实例身份：

```rust
let instance_id = Uuid::new_v4().to_string();

(
    "CODEX_MOBILE_BRIDGE_INSTANCE_ID".to_string(),
    instance_id.clone(),
),
```

构造 `BridgeLaunchPlan` 时加入 `instance_id`。新增测试连续调用两次 `prepare_launch_plan()`，断言两个 plan 的 `instance_id` 不同，且各自 env 中的值与自身 plan 一致。

`apps/bridge-sidecar/src/main.rs` 读取并传入：

```rust
let instance_id = env::var("CODEX_MOBILE_BRIDGE_INSTANCE_ID")
    .unwrap_or_else(|_| Uuid::new_v4().to_string());

let mut state = AppState::new(pairing, EventHub::new(), control_token)
    .with_instance_id(instance_id)
    .with_diagnostics(diagnostics);
```

TypeScript health 类型增加必填字段：

```ts
instanceId: string;
```

- [ ] **Step 7: 运行相关测试**

Run:

```bash
cargo test -p desktop-core bridge_process
cargo test -p bridge-core health_route_reports_bridge_status
cd apps/mobile-pwa && npm test -- --run bridge-protocol.test.ts
```

Expected: PASS；Flexible 仍可回退随机端口，Fixed 明确失败，health 同时返回 version 和 instanceId。

- [ ] **Step 8: 提交**

```bash
git add crates/desktop-core/src/bridge_process.rs crates/bridge-core/src/http_api.rs apps/bridge-sidecar/src/main.rs packages/bridge-protocol/src/status.ts apps/mobile-pwa/src/bridge-protocol.test.ts
git commit -m "feat: add fixed bridge port policy"
```

## Task 2: Keychain 与非敏感配置持久化

**Files:**
- Create: `crates/desktop-core/src/secret_store.rs`
- Create: `crates/desktop-core/src/remote_access_config.rs`
- Modify: `Cargo.toml`
- Modify: `crates/desktop-core/Cargo.toml`
- Modify: `crates/desktop-core/src/lib.rs`
- Test: `crates/desktop-core/src/secret_store.rs`
- Test: `crates/desktop-core/src/remote_access_config.rs`

- [ ] **Step 1: 增加依赖并写 SecretStore 测试**

在 workspace dependencies 增加（`serde_json` 已存在，不重复添加）：

```toml
keyring = { version = "3.6.3", default-features = false, features = ["apple-native"] }
url = "2"
```

在 `desktop-core` dependencies 增加对应 workspace 依赖。先创建测试：

```rust
#[test]
fn memory_secret_store_round_trips_and_deletes_secret() {
    let store = MemorySecretStore::default();

    store.set("cloudflare-tunnel-token", "secret-value").unwrap();
    assert_eq!(
        store.get("cloudflare-tunnel-token").unwrap().as_deref(),
        Some("secret-value")
    );

    store.delete("cloudflare-tunnel-token").unwrap();
    assert_eq!(store.get("cloudflare-tunnel-token").unwrap(), None);
}
```

- [ ] **Step 2: 运行 SecretStore 测试确认失败**

Run:

```bash
cargo test -p desktop-core memory_secret_store_round_trips_and_deletes_secret -- --exact
```

Expected: FAIL，因为模块尚未实现。

- [ ] **Step 3: 实现 Keychain 抽象**

`secret_store.rs` 使用以下公开接口：

```rust
use std::{collections::HashMap, sync::Mutex};

use thiserror::Error;

pub const CLOUDFLARE_TUNNEL_TOKEN_KEY: &str = "cloudflare-tunnel-token";
pub const VAPID_PRIVATE_KEY_KEY: &str = "vapid-private-key";

#[derive(Debug, Error)]
pub enum SecretStoreError {
    #[error("secret store operation failed: {0}")]
    Backend(String),
}

pub trait SecretStore: Send + Sync {
    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError>;
    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError>;
    fn delete(&self, key: &str) -> Result<(), SecretStoreError>;
}

pub struct KeyringSecretStore {
    service: String,
}

impl KeyringSecretStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self { service: service.into() }
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, SecretStoreError> {
        keyring::Entry::new(&self.service, key)
            .map_err(|error| SecretStoreError::Backend(error.to_string()))
    }
}

impl SecretStore for KeyringSecretStore {
    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
        self.entry(key)?
            .set_password(value)
            .map_err(|error| SecretStoreError::Backend(error.to_string()))
    }

    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
        match self.entry(key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SecretStoreError::Backend(error.to_string())),
        }
    }

    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(SecretStoreError::Backend(error.to_string())),
        }
    }
}

#[derive(Default)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemorySecretStore {
    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
        self.values.lock().unwrap().insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
        Ok(self.values.lock().unwrap().get(key).cloned())
    }

    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        self.values.lock().unwrap().remove(key);
        Ok(())
    }
}
```

- [ ] **Step 4: 写配置校验与持久化失败测试**

创建 `remote_access_config.rs` 测试：

```rust
#[test]
fn named_profile_normalizes_hostname_and_round_trips_without_token() {
    let dir = tempfile::tempdir().unwrap();
    let store = RemoteAccessConfigStore::new(dir.path().join("remote-access.json"));
    let profile = NamedTunnelProfile::new(" Codex.Example.COM ", 57324).unwrap();

    store.save(&RemoteAccessPreferences {
        named_tunnel: Some(profile.clone()),
    }).unwrap();

    assert_eq!(profile.hostname, "codex.example.com");
    assert_eq!(store.load().unwrap().named_tunnel, Some(profile));
    let raw = std::fs::read_to_string(dir.path().join("remote-access.json")).unwrap();
    assert!(!raw.to_ascii_lowercase().contains("token"));
}

#[test]
fn named_profile_rejects_url_paths_and_zero_port() {
    assert!(NamedTunnelProfile::new("https://codex.example.com/path", 57324).is_err());
    assert!(NamedTunnelProfile::new("codex.example.com", 0).is_err());
}
```

- [ ] **Step 5: 运行配置测试确认失败**

Run:

```bash
cargo test -p desktop-core named_profile -- --nocapture
```

Expected: FAIL，因为配置类型尚不存在。

- [ ] **Step 6: 实现配置类型和原子写入**

使用以下公开模型：

```rust
#[derive(Debug, Error)]
pub enum RemoteAccessConfigError {
    #[error("local port must be between 1 and 65535")]
    InvalidPort,
    #[error("public hostname must be a hostname without scheme, port, path, query, or fragment")]
    InvalidHostname,
    #[error("remote access configuration I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("remote access configuration JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAccessPreferences {
    pub named_tunnel: Option<NamedTunnelProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedTunnelProfile {
    pub hostname: String,
    pub local_port: u16,
}

impl NamedTunnelProfile {
    pub fn new(hostname: &str, local_port: u16) -> Result<Self, RemoteAccessConfigError> {
        if local_port == 0 {
            return Err(RemoteAccessConfigError::InvalidPort);
        }
        let trimmed = hostname.trim().to_ascii_lowercase();
        if trimmed.contains("://") || trimmed.contains('/') {
            return Err(RemoteAccessConfigError::InvalidHostname);
        }
        let parsed = url::Url::parse(&format!("https://{trimmed}"))
            .map_err(|_| RemoteAccessConfigError::InvalidHostname)?;
        if parsed.host_str() != Some(trimmed.as_str()) || parsed.port().is_some() {
            return Err(RemoteAccessConfigError::InvalidHostname);
        }
        Ok(Self { hostname: trimmed, local_port })
    }

    pub fn local_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.local_port)
    }

    pub fn public_url(&self) -> String {
        format!("https://{}", self.hostname)
    }
}

pub struct RemoteAccessConfigStore {
    path: PathBuf,
}

impl RemoteAccessConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self;
    pub fn load(&self) -> Result<RemoteAccessPreferences, RemoteAccessConfigError>;
    pub fn save(
        &self,
        preferences: &RemoteAccessPreferences,
    ) -> Result<(), RemoteAccessConfigError>;
    pub fn delete(&self) -> Result<(), RemoteAccessConfigError>;
}
```

`RemoteAccessConfigStore::save` 必须写到同目录临时文件、`sync_all` 后 `rename`，不得把 Token 字段加入配置模型；`load` 在文件不存在时返回 `RemoteAccessPreferences::default()`，`delete` 对 NotFound 幂等成功。

- [ ] **Step 7: 导出模块并运行测试**

`lib.rs` 增加：

```rust
pub mod remote_access_config;
pub mod secret_store;

pub use remote_access_config::*;
pub use secret_store::*;
```

Run:

```bash
cargo test -p desktop-core secret_store
cargo test -p desktop-core remote_access_config
```

Expected: PASS。

- [ ] **Step 8: 提交**

```bash
git add Cargo.toml Cargo.lock crates/desktop-core/Cargo.toml crates/desktop-core/src/lib.rs crates/desktop-core/src/secret_store.rs crates/desktop-core/src/remote_access_config.rs
git commit -m "feat: persist named tunnel configuration securely"
```

## Task 3: Named Tunnel 启动、Token 文件和端到端验证

**Files:**
- Create: `crates/desktop-core/src/named_tunnel.rs`
- Modify: `crates/desktop-core/src/lib.rs`
- Test: `crates/desktop-core/src/named_tunnel.rs`

- [ ] **Step 1: 写 launch args 与 Token 文件权限测试**

```rust
#[test]
fn launch_args_use_token_file_without_exposing_token() {
    let args = named_tunnel_launch_args(
        Path::new("/tmp/cloudflare-token"),
        "http://127.0.0.1:57324",
    );

    assert_eq!(
        args,
        vec![
            "tunnel", "--no-autoupdate", "run",
            "--token-file", "/tmp/cloudflare-token",
            "--url", "http://127.0.0.1:57324"
        ]
    );
    assert!(!args.join(" ").contains("secret-token-value"));
}

#[cfg(unix)]
#[test]
fn temporary_token_file_is_mode_0600_and_removed_on_drop() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path;
    {
        let file = TemporarySecretFile::create(dir.path(), "secret-token-value").unwrap();
        path = file.path().to_path_buf();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(!path.exists());
}
```

- [ ] **Step 2: 运行测试确认失败**

Run:

```bash
cargo test -p desktop-core temporary_token_file -- --nocapture
```

Expected: FAIL，因为 `named_tunnel` 模块尚不存在。

- [ ] **Step 3: 实现公开状态、错误和 health identity**

`named_tunnel.rs` 的公开类型固定为：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedTunnelStatus {
    Stopped,
    VerifyingLocal,
    Starting,
    VerifyingPublic,
    Retrying,
    Ready,
    Degraded,
    Failed,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedTunnelFailureKind {
    InvalidConfiguration,
    TokenMissing,
    TokenRejected,
    LocalHealthUnavailable,
    DnsNotReady,
    PublicRouteRejected,
    WrongBridgeInstance,
    NetworkUnavailable,
    ChildExited,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeHealthIdentity {
    pub version: String,
    pub instance_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedTunnelSnapshot {
    pub status: NamedTunnelStatus,
    pub pid: Option<u32>,
    pub local_url: Option<String>,
    pub public_url: Option<String>,
    pub retry_attempt: u8,
    pub failure_kind: Option<NamedTunnelFailureKind>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFailure {
    Dns,
    Timeout,
    Transport,
    HttpStatus(u16),
    InvalidHealthPayload,
}

impl ProbeFailure {
    fn is_deterministic(&self) -> bool {
        matches!(
            self,
            Self::Dns
                | Self::HttpStatus(400 | 401 | 403 | 404)
                | Self::InvalidHealthPayload
        )
    }

    fn public_message(&self) -> String {
        match self {
            Self::Dns => "public hostname DNS is not ready".to_string(),
            Self::Timeout => "public health request timed out".to_string(),
            Self::Transport => "public network is unavailable".to_string(),
            Self::HttpStatus(status) => format!("public health returned HTTP {status}"),
            Self::InvalidHealthPayload => "public health response is invalid".to_string(),
        }
    }
}

#[async_trait]
pub trait NamedTunnelHealthProbe: Send + Sync {
    async fn health(&self, base_url: &str) -> Result<BridgeHealthIdentity, ProbeFailure>;
}

#[derive(Debug, Error)]
pub enum NamedTunnelError {
    #[error("named tunnel is already running")]
    AlreadyRunning,
    #[error("Tunnel Token is missing")]
    TokenMissing,
    #[error("Cloudflare rejected the Tunnel Token")]
    TokenRejected,
    #[error("local Bridge health is unavailable")]
    LocalHealthUnavailable,
    #[error("public hostname DNS is not ready")]
    DnsNotReady,
    #[error("public route returned HTTP {status}")]
    PublicRouteRejected { status: u16 },
    #[error("public health response is invalid")]
    InvalidPublicHealth,
    #[error("public hostname points to a different Bridge instance")]
    WrongBridgeInstance,
    #[error("public network is temporarily unavailable")]
    NetworkUnavailable,
    #[error("cloudflared exited before the tunnel was ready")]
    ChildExited,
    #[error("named tunnel process I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

impl NamedTunnelError {
    fn from_public_probe(error: ProbeFailure) -> Self {
        match error {
            ProbeFailure::Dns => Self::DnsNotReady,
            ProbeFailure::Timeout | ProbeFailure::Transport => Self::NetworkUnavailable,
            ProbeFailure::HttpStatus(408 | 429 | 500..=599) => Self::NetworkUnavailable,
            ProbeFailure::HttpStatus(status) => Self::PublicRouteRejected { status },
            ProbeFailure::InvalidHealthPayload => Self::InvalidPublicHealth,
        }
    }
}
```

`NamedTunnelError::failure_kind()` 必须穷举映射到 `NamedTunnelFailureKind`；外部 Display/detail 只保留上述固定文案或 HTTP status，不携带 Token、完整 cloudflared 行或 reqwest URL。`ProbeFailure::is_deterministic()` 按后续 Step 7 的表实现。

`NamedTunnelConfig` 包含 binary、profile、runtime_dir、startup timeout、poll interval、`max_network_retries: 3`、重试延迟 `[1s, 2s, 4s]`、`runtime_health_interval: 15s` 和独立的 5 秒 runtime health timeout。

- [ ] **Step 4: 实现权限为 0600 的短期文件**

```rust
struct TemporarySecretFile {
    path: PathBuf,
}

impl TemporarySecretFile {
    fn create(runtime_dir: &Path, secret: &str) -> Result<Self, NamedTunnelError> {
        std::fs::create_dir_all(runtime_dir)?;
        let path = runtime_dir.join(format!("cloudflared-token-{}", Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(&path)?;
        file.write_all(secret.as_bytes())?;
        file.sync_all()?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path { &self.path }
}

impl Drop for TemporarySecretFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
```

- [ ] **Step 5: 写公网错误分类与有限重试测试**

使用可注入 `NamedTunnelHealthProbe`，测试至少覆盖：

```rust
#[tokio::test]
async fn dns_failure_is_deterministic_and_is_not_retried() {
    let probe = Arc::new(ScriptedProbe::new(vec![
        Ok(identity("0.1.5", "instance-1")),
        Err(ProbeFailure::Dns),
    ]));
    let mut manager = test_manager(probe.clone());

    let error = manager.start("token-value").await.unwrap_err();

    assert_eq!(error.failure_kind(), NamedTunnelFailureKind::DnsNotReady);
    assert_eq!(probe.public_attempts(), 1);
}

#[tokio::test]
async fn transient_public_failure_retries_three_times_then_stops() {
    let probe = Arc::new(ScriptedProbe::new(vec![
        Ok(identity("0.1.5", "instance-1")),
        Err(ProbeFailure::Timeout),
        Err(ProbeFailure::HttpStatus(503)),
        Err(ProbeFailure::Transport),
        Err(ProbeFailure::Timeout),
    ]));
    let mut manager = test_manager(probe.clone());

    let error = manager.start("token-value").await.unwrap_err();

    assert_eq!(error.failure_kind(), NamedTunnelFailureKind::NetworkUnavailable);
    assert_eq!(probe.public_attempts(), 4);
    assert_eq!(manager.status().status, NamedTunnelStatus::Failed);
}

#[tokio::test]
async fn public_health_must_match_local_version_and_instance() {
    let probe = Arc::new(ScriptedProbe::new(vec![
        Ok(identity("0.1.5", "instance-local")),
        Ok(identity("0.1.5", "instance-other")),
    ]));
    let mut manager = test_manager(probe);

    let error = manager.start("token-value").await.unwrap_err();

    assert_eq!(error.failure_kind(), NamedTunnelFailureKind::WrongBridgeInstance);
}
```

- [ ] **Step 6: 运行重试测试确认失败**

Run:

```bash
cargo test -p desktop-core named_tunnel -- --nocapture
```

Expected: FAIL，因为 manager、probe 和错误分类尚未实现。

- [ ] **Step 7: 实现 NamedTunnelManager 的启动顺序**

`start` 必须严格执行以下代码路径，不允许成功条件退化为“进程仍存活”：

`spawn_cloudflared` 沿用现有 Quick Tunnel 的 process-group/termination helper，设置 `kill_on_drop(true)`，并把 stdout/stderr 设为 piped；`tunnel_output_lines` 必须立即 `take()` 两个 pipe，分别启动 reader task 并返回拥有所有权的 `TunnelOutputMonitor`，不能借用 `Child`，这样验证成功后才能把 child 移入 manager。Manager 同时持有 `output_monitor: Option<TunnelOutputMonitor>`，Ready 后也持续 drain 到有界的脱敏 ring buffer，避免无人读取 pipe 导致 cloudflared 阻塞；stop/fail 时终止 reader tasks。

```rust
pub async fn start(&mut self, token: &str) -> Result<NamedTunnelSnapshot, NamedTunnelError> {
    self.ensure_not_running()?;
    self.state = NamedTunnelState::at(NamedTunnelStatus::VerifyingLocal);

    let local_url = self.config.profile.local_url();
    let public_url = self.config.profile.public_url();
    let local_identity = self.probe.health(&local_url).await
        .map_err(|_| NamedTunnelError::LocalHealthUnavailable)?;

    let token_file = TemporarySecretFile::create(&self.config.runtime_dir, token)?;
    let args = named_tunnel_launch_args(token_file.path(), &local_url);
    self.state.status = NamedTunnelStatus::Starting;
    let mut child = spawn_cloudflared(&self.config.binary, &args)?;
    let mut output = tunnel_output_lines(&mut child);
    self.state.status = NamedTunnelStatus::VerifyingPublic;

    for attempt in 0..=self.config.max_network_retries {
        self.fail_if_child_exited_or_token_rejected(&mut child, &mut output).await?;
        match self.probe.health(&public_url).await {
            Ok(public_identity) => {
                if public_identity != local_identity {
                    return self.fail_start(child, NamedTunnelError::WrongBridgeInstance).await;
                }
                drop(token_file);
                self.child = Some(child);
                self.output_monitor = Some(output);
                self.state = NamedTunnelState::ready(local_url, public_url);
                return Ok(self.snapshot());
            }
            Err(error) if error.is_deterministic() => {
                return self.fail_start(child, NamedTunnelError::from_public_probe(error)).await;
            }
            Err(error) if attempt == self.config.max_network_retries => {
                return self.fail_start(child, NamedTunnelError::from_public_probe(error)).await;
            }
            Err(error) => {
                self.state.status = NamedTunnelStatus::Retrying;
                self.state.retry_attempt = attempt + 1;
                self.state.detail = Some(error.public_message());
                sleep(self.config.retry_delays[attempt as usize]).await;
                self.state.status = NamedTunnelStatus::VerifyingPublic;
            }
        }
    }
    unreachable!("retry loop always returns")
}
```

`max_network_retries = 3` 表示初次公网探测失败后最多再重试 3 次，因此最多执行 4 次公网 health 请求，并完整使用 `[1s, 2s, 4s]` 三段延迟。`ProbeFailure::Dns`、`HttpStatus(400|401|403|404)` 为确定性错误；`Timeout`、transport、`408`、`429`、`500..=599` 为临时错误。解析 cloudflared 输出时，包含 `Invalid Tunnel Secret`、`token is invalid` 或 `Unauthorized` 立即归类 `TokenRejected`。

`TunnelOutputMonitor` 的 `Drop`/`shutdown` 必须取消 reader tasks；所有 `fail_start` 分支在 kill/wait child 后 drop monitor 和 Token file，不能留下 pipe reader task。

- [ ] **Step 8: 实现 stop/terminate 和清理测试**

`stop`、`terminate_now` 必须 kill 子进程、清空 URL、删除尚存 Token 文件。新增测试确认 stop 后 `NamedTunnelStatus::Stopped` 且 shell 测试子进程退出。

- [ ] **Step 9: 增加 Ready 后低频监督与自动恢复**

新增测试：

```rust
#[tokio::test]
async fn runtime_network_loss_degrades_then_recovers_without_respawning_cloudflared() {
    let probe = Arc::new(ScriptedProbe::new(vec![
        Ok(identity("0.1.5", "instance-1")),
        Ok(identity("0.1.5", "instance-1")),
        Err(ProbeFailure::Timeout),
        Ok(identity("0.1.5", "instance-1")),
    ]));
    let mut manager = running_manager(probe);
    let original_pid = manager.status().pid;

    let degraded = manager.refresh_runtime_health(true).await.unwrap();
    assert_eq!(degraded.status, NamedTunnelStatus::Degraded);
    assert_eq!(degraded.pid, original_pid);

    let recovered = manager.refresh_runtime_health(true).await.unwrap();
    assert_eq!(recovered.status, NamedTunnelStatus::Ready);
    assert_eq!(recovered.pid, original_pid);
}
```

Manager 在 startup Ready 时保存本次 `local_identity` 和最后 probe 时间，供运行期比较。`refresh_runtime_health(force)` 规则：

1. 先 `child.try_wait()`；进程退出立即进入 `Failed(ChildExited)`，不自动 respawn、不切 Quick。
2. 进程仍活且距离上次 probe 未到 15 秒、`force=false` 时直接返回当前 snapshot。
3. 公网 health 成功且 version/instance ID 仍匹配，状态恢复/保持 `Ready`。
4. timeout、transport、408、429、5xx 只把状态设为 `Degraded` 并保留 child；`cloudflared` 自己负责底层连接重建，下一次低频 probe 可自动恢复。
5. 401/403/404、DNS NXDOMAIN 或 wrong instance 属于确定性运行时失败，停止 child 并进入 `Failed`。

`Degraded` 不是启动重试循环，不创建新进程，也不改变 Origin。Shell status 刷新和窗口重新打开时调用该方法；即使前端轮询暂停，存活的 `cloudflared` 仍继续其内部重连。

- [ ] **Step 10: 导出并运行测试**

`lib.rs` 增加：

```rust
pub mod named_tunnel;
pub use named_tunnel::*;
```

Run:

```bash
cargo test -p desktop-core named_tunnel -- --nocapture
```

Expected: PASS，且所有测试日志不包含 `token-value`。

- [ ] **Step 11: 提交**

```bash
git add crates/desktop-core/src/named_tunnel.rs crates/desktop-core/src/lib.rs
git commit -m "feat: add verified named tunnel manager"
```

## Task 4: 记录设备配对 Origin，支持迁移后识别旧设备

**Files:**
- Modify: `crates/bridge-core/Cargo.toml`
- Modify: `crates/bridge-core/src/storage.rs:6-14,29-56,58-148,197-250`
- Modify: `crates/bridge-core/src/pairing.rs:100-142,194-208`
- Modify: `crates/bridge-core/src/http_api.rs:121-160,430-520,1490-1551`
- Modify: `apps/mobile-pwa/src/api.ts:24-36,79-91`
- Modify: `apps/mobile-pwa/src/api.test.ts`
- Modify: `apps/desktop-shell/src-tauri/src/main.rs:97-104,275-293`
- Modify: `apps/desktop-shell/src/main.ts:35-40,319-343`
- Test: `crates/bridge-core/src/storage.rs`
- Test: `crates/bridge-core/src/http_api.rs`

- [ ] **Step 1: 写旧数据库迁移与 Origin round-trip 测试**

在 `storage.rs` 新增：

```rust
#[test]
fn migration_adds_paired_origin_to_existing_devices_table() {
    let (_dir, path) = temp_db_path();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE devices (
            device_id TEXT PRIMARY KEY NOT NULL,
            display_name TEXT NOT NULL,
            secret_hash TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            last_seen_at INTEGER NOT NULL,
            revoked_at INTEGER
        );
        "#,
    ).unwrap();
    drop(conn);

    let storage = Storage::open(&path).expect("old database migrates");
    let device = Device {
        device_id: "phone-1".into(),
        display_name: "Phone".into(),
        secret_hash: "hash".into(),
        paired_origin: Some("https://codex.example.com".into()),
        created_at: 1,
        last_seen_at: 1,
        revoked_at: None,
    };
    storage.insert_device(&device).unwrap();

    assert_eq!(storage.device_by_id("phone-1").unwrap(), Some(device));
}
```

- [ ] **Step 2: 运行迁移测试确认失败**

Run:

```bash
cargo test -p bridge-core migration_adds_paired_origin_to_existing_devices_table -- --exact
```

Expected: FAIL，因为 `Device` 没有 `paired_origin`，旧表也不会自动补列。

- [ ] **Step 3: 实现幂等列迁移**

给 `Device` 增加：

```rust
pub paired_origin: Option<String>,
```

在 `Storage::migrate` 的 `CREATE TABLE` 中加入 `paired_origin TEXT`，随后调用：

```rust
fn ensure_column(&self, table: &str, column: &str, definition: &str) -> Result<()> {
    let mut statement = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|value| value == column) {
        self.conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}
```

并在 migration 中执行：

```rust
self.ensure_column("devices", "paired_origin", "TEXT")?;
```

所有 devices INSERT/SELECT 显式加入 `paired_origin`，不能使用 `SELECT *`。

- [ ] **Step 4: 写配对 API Origin 测试**

在 `http_api.rs` 增加测试，POST `/api/pairing/complete` 使用：

```json
{
  "pairingToken": "valid-pairing-token",
  "deviceId": "phone-1",
  "displayName": "Damon phone",
  "deviceSecret": "secret",
  "origin": "https://codex.example.com"
}
```

随后通过 control devices API 断言：

```rust
assert_eq!(devices[0]["pairedOrigin"], json!("https://codex.example.com"));
```

另加非法 Origin 测试，`https://codex.example.com/path` 返回 `400 invalid_request`。

- [ ] **Step 5: 实现只接受 Origin 的校验路径**

在 `crates/bridge-core/Cargo.toml` 增加：

```toml
url = { workspace = true }
```

`PairingCompleteRequest` 增加：

```rust
origin: Option<String>,
```

新增校验：

```rust
fn normalized_pairing_origin(value: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(value) = value else { return Ok(None) };
    let parsed = url::Url::parse(value)
        .map_err(|_| ApiError::BadRequest("origin must be a valid http(s) origin"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return Err(ApiError::BadRequest("origin must not include credentials, path, query, or fragment"));
    }
    Ok(Some(parsed.origin().ascii_serialization()))
}
```

为避免破坏现有测试调用，保留 `register_device` 并转调新方法：

```rust
pub fn register_device(
    &mut self,
    pairing_token: &str,
    device_id: &str,
    display_name: &str,
    device_secret: &str,
) -> Result<DeviceRegistration, PairingError> {
    self.register_device_with_origin(
        pairing_token,
        device_id,
        display_name,
        device_secret,
        None,
    )
}

pub fn register_device_with_origin(
    &mut self,
    pairing_token: &str,
    device_id: &str,
    display_name: &str,
    device_secret: &str,
    paired_origin: Option<String>,
) -> Result<DeviceRegistration, PairingError> {
    let now = self.now();
    let token = self
        .pairing_tokens
        .get(pairing_token)
        .ok_or(PairingError::InvalidToken)?;
    if token.used {
        return Err(PairingError::TokenAlreadyUsed);
    }
    if now >= token.expires_at {
        return Err(PairingError::ExpiredToken);
    }

    let device = Device {
        device_id: device_id.to_string(),
        display_name: display_name.to_string(),
        secret_hash: hash_secret(device_secret),
        paired_origin,
        created_at: now,
        last_seen_at: now,
        revoked_at: None,
    };
    self.storage
        .insert_device(&device)
        .map_err(|_| PairingError::InvalidToken)?;
    self.pairing_tokens
        .get_mut(pairing_token)
        .ok_or(PairingError::InvalidToken)?
        .used = true;
    let (session_token, session_expires_at) =
        self.mint_session_token_for_device_id(device_id);

    Ok(DeviceRegistration {
        device_id: device_id.to_string(),
        session_token,
        session_expires_at,
    })
}
```

原 `register_device` 传 `None`；HTTP handler 调用 `register_device_with_origin`。

- [ ] **Step 6: 手机配对请求提交当前 Origin**

`CompletePairingRequest` 增加：

```ts
origin: string;
```

`completePairing` 的 request body 使用：

```ts
origin: new URL(bridgeUrl).origin,
```

在 `api.test.ts` 断言 fetch body 的 `origin`，并新增 HTTPS 固定 Origin 用例。

- [ ] **Step 7: 桌面设备列表展示 Origin**

Rust `DeviceDto` 和 TypeScript `Device` 增加 `pairedOrigin: string | null`。设备行在 device ID 下显示：

```ts
<span>${escapeHtml(device.pairedOrigin ?? "Origin unknown (paired before v0.1.5)")}</span>
```

固定域名 Ready 时，把不等于当前固定 Origin 的设备标记 `旧 Origin`，只提供现有“撤销”命令，不做自动迁移。

- [ ] **Step 8: 运行测试并提交**

Run:

```bash
cargo test -p bridge-core storage
cargo test -p bridge-core pairing
cargo test -p bridge-core pairing_complete
cd apps/mobile-pwa && npm test -- --run api.test.ts
```

Expected: PASS。

```bash
git add crates/bridge-core/Cargo.toml crates/bridge-core/src/storage.rs crates/bridge-core/src/pairing.rs crates/bridge-core/src/http_api.rs apps/mobile-pwa/src/api.ts apps/mobile-pwa/src/api.test.ts apps/desktop-shell/src-tauri/src/main.rs apps/desktop-shell/src/main.ts
git commit -m "feat: track device pairing origins"
```

## Task 5: Tauri 远程访问协调与 Commands

**Files:**
- Modify: `apps/desktop-shell/src-tauri/src/main.rs:1-43,45-78,114-268,297-337,539-565,692-825`
- Modify: `apps/desktop-shell/src-tauri/Cargo.toml`
- Test: `apps/desktop-shell/src-tauri/src/main.rs`

- [ ] **Step 1: 写远程模式互斥测试**

先把协调规则提取成纯函数并测试：

```rust
#[test]
fn starting_named_tunnel_stops_quick_without_automatic_fallback() {
    assert_eq!(
        transition_remote_access(
            RemoteAccessMode::Quick,
            RemoteAccessAction::StartNamed,
            ActionResult::Failed,
        ),
        RemoteAccessTransition {
            stop_quick: true,
            stop_named: false,
            start_quick: false,
            start_named: true,
            resulting_mode: RemoteAccessMode::NamedFailed,
        }
    );
}

#[test]
fn manual_temporary_action_is_the_only_path_from_named_failure_to_quick() {
    let transition = transition_remote_access(
        RemoteAccessMode::NamedFailed,
        RemoteAccessAction::StartTemporary,
        ActionResult::Succeeded,
    );

    assert!(transition.stop_named);
    assert!(transition.start_quick);
    assert_eq!(transition.resulting_mode, RemoteAccessMode::Quick);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run:

```bash
cargo test -p desktop-shell remote_access -- --nocapture
```

Expected: FAIL，因为协调模型尚不存在。

- [ ] **Step 3: 扩展 ShellState 和状态 DTO**

先实现测试使用的协调模型：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteAccessMode {
    None,
    Quick,
    Named,
    NamedFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteAccessAction {
    StartNamed,
    StartTemporary,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionResult {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RemoteAccessTransition {
    stop_quick: bool,
    stop_named: bool,
    start_quick: bool,
    start_named: bool,
    resulting_mode: RemoteAccessMode,
}

fn transition_remote_access(
    current: RemoteAccessMode,
    action: RemoteAccessAction,
    result: ActionResult,
) -> RemoteAccessTransition {
    let stop_quick = action == RemoteAccessAction::StartNamed
        && current == RemoteAccessMode::Quick;
    let stop_named = matches!(
        current,
        RemoteAccessMode::Named | RemoteAccessMode::NamedFailed
    ) && matches!(
        action,
        RemoteAccessAction::StartNamed
            | RemoteAccessAction::StartTemporary
            | RemoteAccessAction::Stop
    );
    let (start_quick, start_named, resulting_mode) = match (action, result) {
        (RemoteAccessAction::StartNamed, ActionResult::Succeeded) =>
            (false, true, RemoteAccessMode::Named),
        (RemoteAccessAction::StartNamed, ActionResult::Failed) =>
            (false, true, RemoteAccessMode::NamedFailed),
        (RemoteAccessAction::StartTemporary, ActionResult::Succeeded) =>
            (true, false, RemoteAccessMode::Quick),
        (RemoteAccessAction::StartTemporary, ActionResult::Failed) =>
            (true, false, RemoteAccessMode::NamedFailed),
        (RemoteAccessAction::Stop, _) =>
            (false, false, RemoteAccessMode::None),
    };
    RemoteAccessTransition {
        stop_quick,
        stop_named,
        start_quick,
        start_named,
        resulting_mode,
    }
}
```

`ShellState` 改为由 `setup` 使用 app data path 初始化：

```rust
struct ShellState {
    bridge: Mutex<Option<BridgeProcessManager>>,
    quick_tunnel: Mutex<QuickTunnelManager>,
    named_tunnel: Mutex<Option<NamedTunnelManager>>,
    remote_preferences: Mutex<RemoteAccessConfigStore>,
    secret_store: Arc<dyn SecretStore>,
    active_remote_mode: Mutex<RemoteAccessMode>,
    last_pairing_link: Mutex<Option<String>>,
    last_pairing_source: Mutex<Option<PairingLinkSource>>,
    exit_cleanup_started: AtomicBool,
}
```

`PairingLinkSource` 改为 `Local | QuickTunnel | NamedTunnel`。`ShellStatusDto` 增加：

```rust
remote_access: RemoteAccessStatusDto,
```

DTO 至少返回：

```rust
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAccessStatusDto {
    mode: String,
    named_profile: Option<NamedTunnelProfile>,
    named: NamedTunnelSnapshotDto,
    quick: TunnelSnapshotDto,
    fixed_origin_ready: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NamedTunnelSnapshotDto {
    status: String,
    pid: Option<u32>,
    local_url: Option<String>,
    public_url: Option<String>,
    retry_attempt: u8,
    failure_kind: Option<String>,
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAccessPreferencesDto {
    named_profile: Option<NamedTunnelProfile>,
    token_stored: bool,
}
```

Token 是否存在只返回 `tokenStored: bool`，绝不返回 Token 值。

`get_shell_status` 在读取 Named snapshot 前调用 `refresh_runtime_health(false)`；`Degraded` 映射为固定域名“暂时不可达，正在等待 cloudflared 自恢复”，保留固定 Origin 和 pairing link，但不显示 Ready，也不自动启动 Quick Tunnel。确定性 `Failed` 才显示四个失败操作。

- [ ] **Step 4: 增加配置 Commands**

注册以下命令：

```rust
#[tauri::command]
async fn get_remote_access_preferences(
    state: State<'_, ShellState>,
) -> Result<RemoteAccessPreferencesDto, String>;

#[tauri::command]
async fn save_named_tunnel_profile(
    hostname: String,
    local_port: u16,
    token: Option<String>,
    state: State<'_, ShellState>,
) -> Result<RemoteAccessPreferencesDto, String>;

#[tauri::command]
async fn delete_named_tunnel_profile(
    state: State<'_, ShellState>,
) -> Result<(), String>;
```

保存逻辑固定为：

```rust
let profile = NamedTunnelProfile::new(&hostname, local_port)
    .map_err(|error| error.to_string())?;
if let Some(token) = token.filter(|value| !value.trim().is_empty()) {
    state.secret_store
        .set(CLOUDFLARE_TUNNEL_TOKEN_KEY, token.trim())
        .map_err(|error| error.to_string())?;
} else if state.secret_store
    .get(CLOUDFLARE_TUNNEL_TOKEN_KEY)
    .map_err(|error| error.to_string())?
    .is_none()
{
    return Err("Tunnel Token is required for the first setup".to_string());
}
state.remote_preferences.lock().await.save(&RemoteAccessPreferences {
    named_tunnel: Some(profile),
}).map_err(|error| error.to_string())?;
```

删除配置时先停止 Named Tunnel，再删除配置和 Keychain Token。

- [ ] **Step 5: 增加 Bridge 模式切换 helper**

实现：

```rust
enum BridgePortMode {
    Flexible,
    Fixed(u16),
}

async fn ensure_bridge_for_mode(
    app: &AppHandle,
    state: &ShellState,
    mode: BridgePortMode,
) -> Result<BridgeProcessSnapshot, String> {
    let mut bridge = state.bridge.lock().await;
    let current = bridge.as_ref().map(BridgeProcessManager::status);
    let needs_rebuild = match (current.as_ref(), mode) {
        (Some(snapshot), BridgePortMode::Fixed(port)) => {
            snapshot.port != Some(port) || snapshot.port_policy != PortPolicy::Fixed
        }
        (Some(snapshot), BridgePortMode::Flexible) => {
            snapshot.port_policy != PortPolicy::Flexible
                || !matches!(
                    snapshot.status,
                    BridgeProcessStatus::Ready | BridgeProcessStatus::Degraded
                )
        }
        (None, _) => true,
    };
    if needs_rebuild {
        if let Some(manager) = bridge.as_mut() {
            let _ = manager.stop().await;
        }
        let mut config = bridge_config(app)?;
        match mode {
            BridgePortMode::Flexible => config.port_policy = PortPolicy::Flexible,
            BridgePortMode::Fixed(port) => {
                config.preferred_port = Some(port);
                config.port_policy = PortPolicy::Fixed;
            }
        }
        *bridge = Some(BridgeProcessManager::new(config));
    }
    let manager = bridge.as_mut().expect("bridge manager exists");
    if !matches!(manager.status().status, BridgeProcessStatus::Ready | BridgeProcessStatus::Degraded) {
        manager.start().await.map_err(|error| error.to_string())?;
    }
    Ok(manager.status())
}
```

固定端口冲突必须把 `preferred bridge port {port} is unavailable` 映射为 UI 的 `Local port unavailable`，并保留端口号供诊断；不得调用 flexible 重试。

- [ ] **Step 6: 增加 Named Tunnel Commands**

注册：

```rust
start_named_tunnel
retry_named_tunnel
recheck_named_tunnel_health
stop_named_tunnel
start_temporary_tunnel
stop_remote_access
```

`start_named_tunnel` 的关键顺序：

```rust
stop_quick_if_running(&state).await?;
let preferences = state.remote_preferences.lock().await.load()?;
let profile = preferences.named_tunnel.ok_or("Named Tunnel is not configured")?;
ensure_bridge_for_mode(&app, &state, BridgePortMode::Fixed(profile.local_port)).await?;
let token = state.secret_store
    .get(CLOUDFLARE_TUNNEL_TOKEN_KEY)?
    .ok_or("Tunnel Token is missing from Keychain")?;
let snapshot = named_manager(&app, &state, profile).await?
    .start(&token)
    .await
    .map_err(|error| error.to_string())?;
let pairing_link = pairing_link_for_public_url(&state, snapshot.public_url.as_deref()).await?;
set_pairing_link(&state, pairing_link, PairingLinkSource::NamedTunnel).await;
*state.active_remote_mode.lock().await = RemoteAccessMode::Named;
```

错误分支只保存 Named `Failed` snapshot，不得调用 `start_quick_tunnel`。

`start_temporary_tunnel` 是唯一手动降级入口：停止 Named 残留进程；若 Bridge 因固定端口失败则用 Flexible 重建；启动 Quick Tunnel；保留 Named profile 和 Keychain Token。

- [ ] **Step 7: 启动时初始化 state，退出时停止两种 tunnel**

Builder 改为：

```rust
.setup(|app| {
    let app_data_dir = app.path().app_data_dir()?;
    app.manage(ShellState::new(
        RemoteAccessConfigStore::new(app_data_dir.join("remote-access.json")),
        Arc::new(KeyringSecretStore::new("com.codex.mobile.bridge")),
    ));
    Ok(())
})
```

`shutdown_managed_processes` 和 `terminate_managed_processes_now` 都依次处理 Named、Quick、Bridge，不能留下 cloudflared 子进程。

`setup` 还要启动一个 15 秒周期的 Tauri async supervisor：从 `AppHandle` 重新取得 `ShellState`，若 Named manager 处于 `Ready` 或 `Degraded`，调用 `refresh_runtime_health(false)`；它只更新状态，不 respawn、不切换 Origin。应用退出时通过 `exit_cleanup_started`/shutdown cancellation 结束循环，避免后台 task 阻止退出。这样桌面窗口关闭或 WebView 计时器暂停时仍能发现 child exit 和公网恢复。

- [ ] **Step 8: 运行 Tauri Rust 测试并提交**

Run:

```bash
cargo test -p desktop-shell
```

Expected: PASS；互斥规则、固定失败不自动降级、手动临时入口均有测试。

```bash
git add apps/desktop-shell/src-tauri/src/main.rs apps/desktop-shell/src-tauri/Cargo.toml
git commit -m "feat: coordinate fixed and temporary remote access"
```

## Task 6: Mac 三步配置向导与失败操作

**Files:**
- Create: `apps/desktop-shell/src/remote-access.ts`
- Create: `apps/desktop-shell/src/remote-access.test.ts`
- Modify: `apps/desktop-shell/src/main.ts:21-67,101-211,304-395`
- Modify: `apps/desktop-shell/src/styles.css`
- Modify: `apps/desktop-shell/package.json`
- Modify: `apps/desktop-shell/package-lock.json`

- [ ] **Step 1: 为 desktop-shell 增加 Vitest**

`package.json` scripts/devDependencies 增加：

```json
"scripts": {
  "test": "vitest",
  "test:run": "vitest run"
},
"devDependencies": {
  "jsdom": "^25.0.0",
  "vitest": "^3.0.0"
}
```

Run:

```bash
cd apps/desktop-shell && npm install
```

Expected: package lock 更新，无 audit 阻断错误。

- [ ] **Step 2: 写向导状态测试**

创建 `remote-access.test.ts`：

```ts
import { describe, expect, it } from "vitest";
import { nextWizardState, remoteFailureActions, type RemoteWizardState } from "./remote-access";

describe("remote access wizard", () => {
  it("does not advance from connection step without hostname and stored token", () => {
    const state: RemoteWizardState = {
      step: 2,
      hostname: "",
      localPort: 57324,
      tokenStored: false,
      tokenDraft: "",
    };
    expect(nextWizardState(state, { type: "continue" })).toEqual({
      ...state,
      error: "Public Hostname and Tunnel Token are required",
    });
  });

  it("fixed failure exposes manual temporary channel without auto-selecting it", () => {
    expect(remoteFailureActions("failed")).toEqual([
      "retry",
      "edit",
      "diagnostics",
      "start_temporary",
    ]);
  });
});
```

- [ ] **Step 3: 运行测试确认失败**

Run:

```bash
cd apps/desktop-shell && npm test -- --run src/remote-access.test.ts
```

Expected: FAIL，因为 `remote-access.ts` 尚不存在。

- [ ] **Step 4: 实现纯状态模型**

`remote-access.ts` 导出：

```ts
export type WizardStep = 1 | 2 | 3;
export type RemoteFailureAction = "retry" | "edit" | "diagnostics" | "start_temporary";

export interface RemoteWizardState {
  step: WizardStep;
  hostname: string;
  localPort: number;
  tokenStored: boolean;
  tokenDraft: string;
  error?: string;
}

export function remoteFailureActions(status: string): RemoteFailureAction[] {
  return status === "failed"
    ? ["retry", "edit", "diagnostics", "start_temporary"]
    : [];
}

export function nextWizardState(
  state: RemoteWizardState,
  action: { type: "continue" | "back" | "edit" },
): RemoteWizardState {
  if (action.type === "back" || action.type === "edit") {
    return { ...state, step: Math.max(1, state.step - 1) as WizardStep, error: undefined };
  }
  if (state.step === 2 && (!state.hostname.trim() || (!state.tokenStored && !state.tokenDraft.trim()))) {
    return { ...state, error: "Public Hostname and Tunnel Token are required" };
  }
  return { ...state, step: Math.min(3, state.step + 1) as WizardStep, error: undefined };
}
```

另外导出 `renderRemoteAccessPanel(model)`，所有 hostname、URL、错误详情和状态文本都必须经过现有 `escapeHtml`；HTML 中 Token input 必须永远使用 `value=""`；`tokenStored` 只显示“已安全保存在 Keychain”。

- [ ] **Step 5: 在主界面接入完整 Remote Access 面板**

用一个 full-width panel 替换现有“远程链接 Beta”卡片。第一视图使用分段选择：`固定域名` / `临时通道`，不嵌套卡片。

三步内容固定为：

1. `Create Tunnel`: 显示 Cloudflare Dashboard 链接、`http://localhost:<port>` Origin Service、复制按钮。
2. `Connect Bridge`: hostname、Tunnel Token password input、local port number input。
3. `Verify`: 依次显示 Local Bridge、Cloudflare connection、Public health、Same Bridge instance 四项状态。

失败时只渲染四个明确命令：

```html
<button data-action="retry-named-tunnel">重试</button>
<button data-action="edit-named-tunnel">修改配置</button>
<button data-action="copy-diagnostics">查看诊断</button>
<button data-action="start-temporary-tunnel">启动临时通道</button>
```

临时通道运行时显示：`当前为临时 URL；锁屏通知已暂停`。不得把固定域名状态继续显示为 Ready。

Named `Degraded` 时显示非阻塞状态条和 `立即重新检测` 按钮；该按钮调用 runtime health force refresh，不重启进程。只有进入 `Failed` 后才显示 Retry/Edit/Diagnostics/Start temporary 四个命令。

- [ ] **Step 6: 绑定 Tauri commands**

新增 action 映射：

```ts
if (action === "save-named-profile") {
  await invoke("save_named_tunnel_profile", {
    hostname: wizard.hostname,
    localPort: wizard.localPort,
    token: wizard.tokenDraft || null,
  });
}
if (action === "start-named-tunnel" || action === "retry-named-tunnel") {
  await runAction("固定域名验证完成", () => invoke("start_named_tunnel"));
}
if (action === "recheck-named-tunnel-health") {
  await runAction("固定域名状态已刷新", () => invoke("recheck_named_tunnel_health"));
}
if (action === "start-temporary-tunnel") {
  await runAction("临时通道已启动", () => invoke("start_temporary_tunnel"));
}
```

保存成功后立即清空内存中的 `tokenDraft`；render、notice、error 和 diagnostics 里不得保留 Token。

- [ ] **Step 7: 增加响应式样式并运行测试/build**

CSS 要求：stepper 固定三列；表单在窄窗口变一列；长 hostname 和错误文本可换行；按钮使用现有 8px 以下圆角；状态列表不使用嵌套卡片。

Run:

```bash
cd apps/desktop-shell && npm test -- --run
npm run build
```

Expected: PASS；TypeScript 无错误，向导测试通过。

- [ ] **Step 8: 提交**

```bash
git add apps/desktop-shell/src/remote-access.ts apps/desktop-shell/src/remote-access.test.ts apps/desktop-shell/src/main.ts apps/desktop-shell/src/styles.css apps/desktop-shell/package.json apps/desktop-shell/package-lock.json
git commit -m "feat: add named tunnel setup wizard"
```

## Task 7: 诊断脱敏、版本升级与 Phase 1 回归

**Files:**
- Modify: `crates/desktop-core/src/diagnostics_bundle.rs:124-180,276-352`
- Modify: `apps/desktop-shell/src-tauri/src/main.rs:297-337,494-537`
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

- [ ] **Step 1: 写 Cloudflare Token 脱敏测试**

```rust
#[test]
fn redacts_cloudflare_tunnel_credentials_and_secret_file_paths() {
    let input = concat!(
        "CLOUDFLARE_TUNNEL_TOKEN=eyJhIjoiMTIzIn0.long-secret\n",
        "cloudflared tunnel run --token-file /Users/damon/token-file"
    );

    let redacted = redact_sensitive_text(input);

    assert!(!redacted.contains("eyJhIjoiMTIzIn0.long-secret"));
    assert!(!redacted.contains("/Users/damon/token-file"));
    assert!(redacted.contains("[REDACTED]"));
    assert!(redacted.contains("[LOCAL_PATH]"));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run:

```bash
cargo test -p desktop-core redacts_cloudflare_tunnel_credentials_and_secret_file_paths -- --exact
```

Expected: FAIL，Cloudflare assignment 尚未纳入脱敏。

- [ ] **Step 3: 扩展脱敏与诊断状态**

在 `redact_sensitive_text` 增加：

```rust
redacted = redact_assignment_value(&redacted, "CLOUDFLARE_TUNNEL_TOKEN");
redacted = redact_after_marker(&redacted, "--token ", false);
```

诊断只允许记录：mode、hostname、local port、Named failure kind、retry count、public health status、cloudflared exit category。禁止记录 Token、完整 token file 内容、VAPID secret 或 PushSubscription keys。

- [ ] **Step 4: 更新 dogfood 清单**

在 `docs/dogfood-qa-checklist.md` 增加固定域名验收：

```markdown
- [ ] 配置 Named Tunnel 后重启 Mac App，hostname/port 保留，Token 输入框为空且显示 Keychain 已保存。
- [ ] 固定端口被占用时显示 Local port unavailable，不切换随机端口。
- [ ] Token 错误、DNS 错误不持续重试。
- [ ] 临时网络错误最多重试 3 次后停止。
- [ ] Ready 后短暂断网显示 Degraded；网络恢复后同一 cloudflared PID 自动回到 Ready。
- [ ] Ready 后 cloudflared 子进程退出会进入 Failed，不自动 respawn 或切换 Quick。
- [ ] 固定域名失败后不会自动启动 Quick Tunnel。
- [ ] 点击“启动临时通道”后固定配置仍保留，页面明确显示锁屏通知暂停。
- [ ] 固定域名重新配对后，Devices 可识别旧 Origin 并允许撤销。
```

- [ ] **Step 5: 统一升级版本到 0.1.5**

把 `VERSION` 和 `scripts/check-version-sync.sh` 检查的所有 manifest 改为 `0.1.5`，并让两个 `package-lock.json` 顶层 package version 同步。不要修改第三方 dependency version。

Run:

```bash
scripts/check-version-sync.sh
```

Expected: `Version 0.1.5 is synchronized across desktop, sidecar, and PWA manifests.`

- [ ] **Step 6: 运行完整自动回归**

Run:

```bash
cargo fmt --all -- --check
cargo test --workspace
cd apps/mobile-pwa && npm test -- --run && npm run build
cd ../desktop-shell && npm test -- --run && npm run build
cd ../.. && scripts/check-release-gate.sh --channel dev
```

Expected: 全部 PASS。

- [ ] **Step 7: 验证 bundled cloudflared 参数**

Run:

```bash
apps/desktop-shell/src-tauri/resources/bin/cloudflared tunnel --no-autoupdate run --help
```

Expected: 命令成功显示 run help，且同时包含 `--token-file` 和 `--url`；另用不存在的测试 token-file 做 parser smoke 时不得出现 `flag provided but not defined`。执行日志不得打印真实 Token。

- [ ] **Step 8: 构建试用 DMG 并人工验证**

Run:

```bash
cd apps/desktop-shell && npm run tauri:build
```

Expected: `target/release/bundle/dmg/` 生成 `0.1.5` DMG。人工完成：固定域名配置、重启恢复、固定二维码重新配对、错误 Token、DNS 错误、三次网络重试和手动 Quick Tunnel。

- [ ] **Step 9: 提交 Phase 1**

```bash
git add crates/desktop-core/src/diagnostics_bundle.rs apps/desktop-shell/src-tauri/src/main.rs docs/dogfood-qa-checklist.md VERSION crates/bridge-core/Cargo.toml crates/desktop-core/Cargo.toml apps/bridge-sidecar/Cargo.toml apps/desktop-shell/src-tauri/Cargo.toml apps/desktop-shell/src-tauri/tauri.conf.json apps/desktop-shell/package.json apps/desktop-shell/package-lock.json apps/mobile-pwa/package.json apps/mobile-pwa/package-lock.json
git commit -m "release: prepare fixed tunnel beta v0.1.5"
```

## Phase 1 验收门槛

- Named Tunnel Ready 必须同时满足本地 health、cloudflared 子进程、公网 health、version 和 instance ID 一致。
- Ready 后由后台 supervisor 低频探测；临时网络中断保留同一 child 并可恢复，child exit/确定性错误显式 Failed。
- 固定端口冲突明确失败，不回退随机端口。
- 确定性错误不重试；临时错误在初次失败后最多重试 3 次（公网 health 总请求最多 4 次）；失败后不自动切 Quick。
- 只有用户点击“启动临时通道”才会切换 Quick Tunnel。
- Tunnel Token 只存在 Keychain 和短期 `0600` 文件，配置、命令行、日志、诊断和 Tauri DTO 中均不存在明文。
- Named 与 Quick 同一时刻最多运行一个；退出 App 后两个 cloudflared 子进程都被清理。
- 固定 Origin 重新配对后可识别并撤销旧 Origin 设备。
- `cargo test --workspace`、两个前端 build、版本同步和 dev release gate 全部通过。
