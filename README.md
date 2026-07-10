# Codex Mobile Bridge

macOS 优先的 ChatGPT/Codex Desktop 手机桥接 MVP。电脑侧运行 Rust sidecar，手机侧打开同一局域网内的 PWA，用来查看会话流、发送文本回复，并处理可捕获的审批请求。

## MVP 范围

- 桌面端优先支持新版 ChatGPT Desktop App，并兼容仍叫 Codex 的旧安装；通过 CDP target 发现和注入 bridge 脚本连接 app-server JSON-RPC。
- 手机端是 PWA，由 sidecar 静态托管，扫码或复制 URL 打开。
- 设备配对为本机长期绑定；MVP 安全策略是“已配对手机等同本机用户审批”。
- 第一版只做局域网直连；公网 tunnel、云端账号、多租户、Web Push、原生 App 和复杂授权策略不在当前范围。

## 开发命令

```bash
cargo test --workspace -- --nocapture
cargo check --workspace

cd apps/mobile-pwa
npm test -- --run
npm run test:run
npm run build
```

桌面壳开发：

```bash
cargo build -p bridge-sidecar
(cd apps/mobile-pwa && npm ci && npm run build)

cd apps/desktop-shell
npm ci
npm run build
npm run tauri:dev
```

桌面壳当前是产品化 Mac App 的 scaffold：提供 ChatGPT/Codex 检测/启动、Bridge start/stop、手机配对二维码/链接、Quick Tunnel Beta、设备撤销和本地诊断入口。开发模式会自动向上定位仓库根目录，并默认读取：

- `CODEX_MOBILE_BRIDGE_SIDECAR_BIN`，未设置时使用 `target/debug/bridge-sidecar`。
- `CODEX_MOBILE_BRIDGE_PWA_DIR`，未设置时使用 `apps/mobile-pwa/dist`。
- `CODEX_MOBILE_BRIDGE_ADVERTISED_HOST`，未设置时自动尝试 Wi-Fi/LAN IP。

桌面壳打包前先准备 bundle resources：

```bash
cd apps/desktop-shell
npm run tauri:build
```

`tauri:build` 会先运行 `prepare:bundle`，构建 release sidecar 和 mobile PWA，并复制到 `apps/desktop-shell/src-tauri/resources/`。这些生成物不会提交到仓库；Tauri 打包时会把它们放进 App resources，运行时桌面壳会优先使用 bundle 内资源，找不到时才回退到开发路径。

GitHub Actions 可手动生成内部试用包：运行 **Desktop build** workflow，`channel` 选 `dev` 或 `beta`，`bundles` 选 `dmg`。

## 启动

先构建 PWA：

```bash
cd apps/mobile-pwa
npm run build
```

再启动 sidecar：

```bash
cd ../..
CODEX_MOBILE_BRIDGE_DEBUG_PORT=9229 cargo run -p bridge-sidecar
```

默认配置：

- `CODEX_MOBILE_BRIDGE_BIND=0.0.0.0:57324`
- `CODEX_MOBILE_BRIDGE_DEBUG_PORT=9229`
- `CODEX_MOBILE_BRIDGE_DB=bridge.sqlite`
- `CODEX_MOBILE_BRIDGE_PWA_DIR=apps/mobile-pwa/dist`

启动后终端会打印：

- 本机监听地址。
- PWA 静态目录。
- `PWA pairing URL`，形如 `http://<lan-ip>:57324/?pairingToken=...&bridgeUrl=...`。
- `QR text`，内容同 pairing URL，可复制给二维码工具。
- 本机 control token，用于后续手动启动新的 pairing token。

## ChatGPT/Codex Desktop 要求

OpenAI 将 Codex app 并入新版 ChatGPT Desktop 后，用户机器上可能存在两种名称：新版 `ChatGPT.app`，或仍叫 `Codex.app` 的旧安装。本项目同时兼容这两种名称，但会排除 `ChatGPT Classic`。目标桌面应用必须以 remote debugging port 启动，端口和 `CODEX_MOBILE_BRIDGE_DEBUG_PORT` 一致。当前 MVP 默认端口是 `9229`。

sidecar 的诊断顺序：

1. 查询 CDP targets。
2. 选择 ChatGPT/Codex page target。
3. 注入 `window.__codexMobileBridge.rpc`。
4. 通过 app-server RPC 检查 `thread/list`，如已有 thread 再检查 `thread/turns/list`。
5. 根据结果返回 `writable`、`read_only` 或明确降级原因。

## 常见降级状态

- `codex_not_running`：sidecar 尚未跑过有效诊断，或 ChatGPT/Codex 未启动。
- `cdp_unavailable`：debug port 不可达，检查 ChatGPT/Codex 启动参数和端口。
- `target_not_found`：CDP 可达，但没有识别到 ChatGPT/Codex page target。
- `inject_failed`：找到 ChatGPT/Codex target，但页面内未发现可用 app-server client。
- `rpc_unavailable`：注入成功但基础 app-server RPC 不可用。
- `read_only`：可读取会话，但文本回写能力不可用，手机端应禁用文本回写。
- `writable`：Desktop bridge RPC 可用，手机端允许向已选 thread 发送文本。

## 局域网安全边界

- sidecar 默认绑定 `0.0.0.0:57324`，同一局域网内设备可访问。
- 配对 URL 内的一次性 `pairingToken` 有效期有限，使用后失效。
- 会话数据 API 和 WebSocket 都需要已配对设备的 session token。
- MVP 不做账号体系、云端鉴权、风险分级或二次确认；不要把端口直接暴露到公网。

## 手动烟测

1. 启动带 remote debugging port 的 ChatGPT 或 Codex Desktop。
2. 运行 `cargo run -p bridge-sidecar`，访问 `/api/health`，确认返回 `writable` 或明确降级状态。
3. 用手机或同网浏览器打开终端打印的 PWA pairing URL，完成配对并刷新页面，确认仍保持登录。
4. 选择 thread，发送文本，确认 ChatGPT/Codex Desktop 对应 thread 继续执行。
5. 触发需要确认的命令或文件编辑，确认 PWA 出现审批卡片，批准或拒绝后桌面任务继续。

## 试用与发布

- 内部同事试用流程见 [docs/dogfood-qa-checklist.md](docs/dogfood-qa-checklist.md)。
- dev / beta / stable 发布门禁见 [docs/release-gates.md](docs/release-gates.md)。
- `scripts/check-release-gate.sh --channel stable` 会阻止未签名、未公证、缺 updater metadata 或把 Quick Tunnel 当稳定远程能力的公开发布。

## 参考

- CodexPlusPlus mobile relay 和 CDP bridge 方向验证：<https://github.com/BigPizzaV3/CodexPlusPlus>
