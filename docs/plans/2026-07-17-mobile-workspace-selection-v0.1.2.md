# v0.1.2 手机新建会话工作目录选择实施计划

日期：2026-07-17
分支：`feature/codex-mobile-bridge-mvp`
状态：工程评审完成，待实施

## 目标

手机端新建会话时，用户必须从现有 ChatGPT/Codex 会话已经使用过的工作目录中选择一个目录。Bridge 在创建前重新读取最新会话列表并校验，彻底移除当前固定使用 `cwd: "/"` 的行为。

成功标准：

- 手机端可以看到安全、去重、排序后的工作目录列表。
- 当前会话目录合法时默认选中；仅有一个目录时自动选中；多目录且无上下文时要求手选。
- Bridge 拒绝根目录、相对路径、不存在目录、普通文件及不在最新 Codex 会话集合中的目录。
- 创建请求精确传递同一个 `cwd` 和 `workspaceRoots: [cwd]` 给 Desktop host bridge。
- 工作目录错误不会再被前端误报为“配对链接过期”。
- 自动测试全部通过后，再完成真实 ChatGPT/Codex Desktop 与手机端端到端 QA。

## 数据流

```text
手机打开“新建会话”
        |
        v
GET /api/workspaces -- Bearer session
        |
        v
Bridge 调用最新 thread/list
        |
        v
统一 WorkspacePolicy
  |- 提取 thread.cwd
  |- 必须是绝对路径
  |- 拒绝 /
  |- 必须存在且是目录
  `- 去重并稳定排序
        |
        v
PWA 根据当前会话上下文选择默认项
        |
        v
POST /api/sessions { text, attachments, cwd }
        |
        v
Bridge 再次 thread/list + 同一策略精确校验 cwd
        |
        v
codex-mobile/start-conversation
  cwd = selected cwd
  workspaceRoots = [selected cwd]
```

## 已确认决策

1. 工作目录只来源于现有 Codex 会话的 `cwd`，不允许手机输入任意路径。
2. 不建设完整的 macOS 文件夹授权、持久化白名单和撤销授权系统。
3. 列表展示与提交校验共用同一个 Rust workspace policy。
4. `GET /api/workspaces` 和 `POST /api/sessions` 都需要现有 Bearer session 鉴权。
5. 创建前必须重新读取最新 `thread/list`，不能信任前端或旧缓存。
6. API 错误返回稳定的 `{ code, error }`，前端按 `code` 决定恢复动作。
7. PWA 使用统一 `withSessionRefresh()` 完成一次 401/403 刷新重试。
8. 验证策略选择 A：自动测试通过后，再做真实 Desktop 与手机端 QA。

## What already exists

- `SessionSnapshot.cwd` 已同时存在于 Rust 与 TypeScript 协议，可直接作为工作目录来源，不需要新增会话元数据抓取链路。
- `CodexAdapter::list_threads()` 已调用 `thread/list`，`GET /api/sessions` 也已经走真实 Desktop adapter。
- `POST /api/sessions` 已支持文本和图片附件，并通过 `codex-mobile/start-conversation` 创建真实 Desktop 会话。
- PWA 已有新建会话 sheet、当前会话选择状态、发送中状态和错误区域，可在原组件内增加目录选择而不新建页面。
- 配对 session 的 Bearer 鉴权、设备 session 刷新、API 单元测试和 React/Vitest 测试基础设施已经存在。
- 仓库已有统一版本文件和版本同步检查脚本，`v0.1.2` 应沿用该流程。

## NOT in scope

- macOS 原生文件夹选择器和 security-scoped bookmark：本版本只复用 Codex 已经使用过的目录。
- 手机手输任意绝对路径：无法建立可靠授权边界，明确禁止。
- 首次从手机打开一个从未在 Codex 中出现过的新项目：留到完整目录授权版本。
- 项目目录别名、收藏、分组、搜索和最近使用排序：当前目录数量不足以证明需要额外状态。
- 跨设备同步工作目录白名单：当前 Bridge 是单机 sidecar，没有云端账号控制面。
- 缓存 `thread/list` 来减少创建前校验：创建会话是低频操作，安全新鲜度优先于一次本地 RPC 成本。

## 工程评审结论

### Architecture Review

1. **P1：移除根目录默认值。** 工作目录来源改为现有会话的 `cwd`，避免新任务落入错误项目或获得过大的文件系统视野。
2. **P1：Bridge 是最终安全边界。** 手机选择值只能视为不可信输入，提交时必须重新读取最新线程并精确匹配。
3. **P2：新增独立工作目录接口。** `GET /api/workspaces` 由统一策略生成，避免让 PWA 自己从 session 列表重复实现过滤规则。

### Code Quality Review

1. **P1：共享协议类型。** Rust/TS 同时增加 `WorkspaceOption`、`ApiErrorCode` 和带 `cwd` 的创建请求契约。
2. **P1：结构化错误。** 不能继续把所有 HTTP 400 都解释成配对链接过期。
3. **P2：统一认证重试。** 抽取 `withSessionRefresh()`，迁移 sessions、events、approvals、create 和 workspaces，避免第五份刷新逻辑。

### Performance Review

没有新增阻塞问题。

- 打开新建 sheet 时调用一次 `thread/list`，创建前再调用一次；该交互低频且第二次调用是安全校验要求。
- 目录过滤是本机路径检查，复杂度为 `O(n log n)`，其中 `n` 是去重后的会话目录数。
- 不增加轮询、后台扫描或目录递归遍历。
- 不使用缓存替代创建前新鲜校验；如果未来线程数量达到明显性能瓶颈，再为 Codex adapter 增加分页或短 TTL 只读缓存，提交校验仍保持新鲜读取。

## 测试覆盖图

```text
CODE PATHS                                             USER FLOWS
[+] workspace_policy.rs                               [+] 打开新建会话 sheet
  |- [GAP] 合法绝对目录保留                              |- [GAP] 当前会话 cwd 合法 -> 默认选中
  |- [GAP] 根目录 / 拒绝                                 |- [GAP] 只有一个目录 -> 自动选中
  |- [GAP] 相对路径拒绝                                  |- [GAP] 多目录无上下文 -> 必须手选
  |- [GAP] 普通文件拒绝                                  |- [GAP] 空列表 -> 禁用提交并说明原因
  |- [GAP] 不存在目录拒绝                                `- [GAP] 加载失败 -> 可重试且保留草稿
  `- [GAP] 去重 + 稳定排序

[+] GET /api/workspaces                               [+] 提交新会话
  |- [GAP] Bearer session 有效 -> 200                    |- [GAP] [->E2E] 选择项目并创建真实会话
  |- [GAP] 未认证 -> 401/403                             |- [GAP] 双击提交只创建一次
  |- [GAP] adapter thread/list 失败 -> 502                |- [GAP] session 过期 -> 刷新一次后成功
  `- [GAP] 异常线程数据 -> 安全过滤                        |- [GAP] workspace 失效 -> 保留草稿并重载列表
                                                        `- [GAP] 篡改 cwd -> 明确拒绝

[+] POST /api/sessions                                [+] 错误恢复
  |- [GAP] 缺少 cwd -> workspace_required                |- [GAP] workspace 错误不显示“配对过期”
  |- [GAP] cwd 不在最新集合 -> workspace_not_allowed      `- [CRITICAL GAP] 配对 400 仍显示配对错误
  |- [GAP] cwd 已消失/变成文件 -> workspace_unavailable
  `- [GAP] 合法 cwd -> adapter.start_thread(cwd, ...)

[+] codex_rpc.rs
  |- [GAP] start-conversation 精确传递 cwd
  `- [GAP] workspaceRoots 精确等于 [cwd]

[+] api.ts / App.tsx
  |- [GAP] WorkspaceOption 与 ApiErrorCode 解析
  |- [GAP] withSessionRefresh 最多重试一次
  |- [GAP] 非认证错误不刷新
  `- [GAP] 失败后文本、附件和目录选择不丢失

COVERAGE: 0/30 planned paths currently implemented
GAPS: 30 | E2E: 1 automated integration path + 3 real-device checks | EVAL: 0
```

说明：这些是新功能尚未实现前的计划缺口。实现时测试与代码同批提交，不把测试推迟到后续版本。

## 自动测试计划

### Rust

- Workspace policy 单元测试：合法目录、`/`、相对路径、普通文件、目录消失、重复路径和稳定排序。
- `GET /api/workspaces` 路由测试：鉴权、成功响应、adapter 失败、安全过滤。
- `POST /api/sessions` 路由测试：缺少 `cwd`、篡改路径、失效路径和成功创建。
- Adapter 测试：`codex-mobile/start-conversation` 精确携带 `cwd` 与 `workspaceRoots`。
- API 回归测试：结构化 workspace 400 不改变配对接口既有 400 语义。

### TypeScript / React

- 协议解析测试：合法与非法 `WorkspaceOption`、已知与未知错误码。
- API 测试：workspaces 请求、创建请求携带 `cwd`、结构化错误解析。
- `withSessionRefresh()` 测试：401/403 只刷新并重试一次；400/500 不刷新；刷新失败原样上抛。
- 新建会话 UI 测试：三种默认选择规则、空列表、加载失败、重试、双击提交和失败保留草稿。
- 强制回归测试：workspace 错误不能显示为配对过期；真实配对 400 仍显示配对错误。

### 验证命令

```bash
cargo test -p bridge-core
cargo test --workspace
cd apps/mobile-pwa && npm test -- --run
cd apps/mobile-pwa && npm run build
cd apps/desktop-shell && npm test -- --run
./scripts/check-version-sync.sh
```

## 真实端到端 QA

自动测试全部通过后执行：

1. 从手机选择真实项目目录，新建任务并确认 ChatGPT/Codex Desktop 收到首条消息且开始回复。
2. 在 Desktop 中确认新线程的 `cwd` 与手机选择完全一致，没有落入 `/` 或其他项目。
3. 通过调试请求篡改 `cwd` 为未授权目录，Bridge 必须拒绝且 Desktop 不创建线程。
4. 让已展示目录在提交前失效，手机应保留草稿、显示可恢复错误并刷新目录列表。
5. 分别使用局域网和 Quick Tunnel 完成一次创建，确认认证刷新和错误展示一致。

## 生产失败模式

| 新路径 | 现实失败方式 | 测试 | 错误处理 | 用户可见结果 |
|---|---|---|---|---|
| 获取 workspaces | Desktop 未启动或 CDP 注入失效 | adapter 失败路由测试 | 转为稳定 API 错误 | 显示加载失败和重试，不清空草稿 |
| 目录策略 | 线程携带 `/`、相对路径或脏数据 | policy 边界测试 | 静默过滤不安全项 | 只看到安全目录；空列表时说明无法创建 |
| 创建前校验 | 列表展示后目录被删除 | 失效目录路由测试 | 返回 `workspace_unavailable` | 保留草稿并要求重选 |
| 创建前校验 | 请求被篡改为其他绝对路径 | 篡改路径路由测试 | 返回 `workspace_not_allowed` | 明确拒绝，不创建 Desktop 线程 |
| Desktop RPC | `start-conversation` 超时或失败 | adapter 错误测试 | 映射为上游错误 | 创建失败，草稿和选择仍保留 |
| session 刷新 | token 过期且刷新成功 | PWA 刷新测试 | 最多重试一次 | 用户无感继续 |
| session 刷新 | 刷新也失败 | PWA 刷新失败测试 | 停止重试并更新连接状态 | 提示重新连接，不形成请求循环 |
| 错误解析 | 旧 Bridge 返回纯文本错误 | API 兼容测试 | 回退到通用错误信息 | 不崩溃，不误判配对状态 |
| 快速重复提交 | 用户连续点击创建 | React 交互测试 | `sending` 锁定按钮 | 只创建一个线程 |

关键缺口：0。每个失败模式都有计划测试、错误处理和可见恢复路径。

## 并行实施策略

| Step | 模块 | 依赖 |
|---|---|---|
| 协议与 workspace policy | `packages/bridge-protocol/`, `crates/bridge-core/` | - |
| Adapter 传递 cwd | `crates/bridge-core/` | 协议与 policy |
| HTTP API 与结构化错误 | `crates/bridge-core/` | 协议与 policy、Adapter |
| PWA API 与认证刷新 | `apps/mobile-pwa/` | 协议 |
| PWA 新建会话 UI | `apps/mobile-pwa/` | PWA API |
| 版本、构建与 QA | workspace scripts / all apps | 全部实现完成 |

并行 lanes：

- Lane A：Rust 协议/policy -> Adapter -> HTTP API，均修改 `bridge-core`，顺序执行。
- Lane B：TypeScript 协议 -> PWA API -> PWA UI，均修改 `mobile-pwa`，顺序执行。
- Lane C：测试计划、QA 清单和版本检查，可在 A/B 稳定后收尾。

执行顺序：A 与 B 的协议阶段可以并行，但共享契约容易漂移；本次工作区已有大量未提交改动，采用单工作区顺序实施更安全。完成 Rust 契约后实现 PWA，再统一跑全量测试。

## Implementation Tasks

- [ ] **T1 (P1, human: ~1h / Codex: ~10min)** — Protocol — 增加 `WorkspaceOption`、`ApiErrorCode` 和创建请求 `cwd`
  - Surfaced by: Code Quality — Rust/TS 共享协议必须避免漂移。
  - Files: `crates/bridge-core/src/protocol.rs`, `packages/bridge-protocol/src/protocol.ts`
  - Verify: Rust/TS 协议单元测试。

- [ ] **T2 (P1, human: ~2h / Codex: ~20min)** — Workspace Policy — 实现统一的目录提取与校验策略
  - Surfaced by: Architecture — 前端不是安全边界，展示与提交必须共用规则。
  - Files: `crates/bridge-core/src/workspace.rs`, `crates/bridge-core/src/lib.rs`
  - Verify: 合法、根目录、相对路径、文件、失效、去重和排序测试。

- [ ] **T3 (P1, human: ~1h / Codex: ~10min)** — Desktop Adapter — 将选择目录传入 `start-conversation`
  - Surfaced by: Architecture — 当前 adapter 固定发送 `/`。
  - Files: `crates/bridge-core/src/codex_rpc.rs`, `crates/bridge-core/src/cdp.rs`
  - Verify: 捕获 RPC 参数并断言 `cwd` 与 `workspaceRoots`。

- [ ] **T4 (P1, human: ~2h / Codex: ~25min)** — Phone API — 新增 workspaces 接口、创建前校验和结构化错误
  - Surfaced by: Architecture + Code Quality — 服务端必须新鲜校验并返回可恢复错误。
  - Files: `crates/bridge-core/src/http_api.rs`
  - Verify: 路由鉴权、adapter 失败、篡改、失效及成功创建测试。

- [ ] **T5 (P1, human: ~1.5h / Codex: ~20min)** — PWA API — 增加目录 API、错误解析及统一 session refresh
  - Surfaced by: Code Quality — 当前认证重试重复且所有 400 容易误报配对过期。
  - Files: `apps/mobile-pwa/src/api.ts`, `apps/mobile-pwa/src/api.test.ts`
  - Verify: API/Vitest 覆盖一次刷新、非认证错误和旧错误兼容。

- [ ] **T6 (P1, human: ~2h / Codex: ~25min)** — PWA UI — 在新建会话 sheet 中加入工作目录选择和恢复状态
  - Surfaced by: Product flow — 必须防止跨项目误建并保留失败草稿。
  - Files: `apps/mobile-pwa/src/App.tsx`, `apps/mobile-pwa/src/App.test.tsx`, `apps/mobile-pwa/src/styles.css`
  - Verify: 三种默认规则、空态、失败、重试、双击和草稿保留测试。

- [ ] **T7 (P1, human: ~1h / Codex: ~15min)** — Regression Suite — 补齐配对错误与 workspace 错误回归测试
  - Surfaced by: Test Review — 现有 400 映射会把业务错误显示成配对链接过期。
  - Files: `apps/mobile-pwa/src/App.test.tsx`, `apps/mobile-pwa/src/api.test.ts`, `crates/bridge-core/src/http_api.rs`
  - Verify: 两类 400 分别显示正确恢复动作。

- [ ] **T8 (P1, human: ~1h / Codex: ~15min)** — Release — 升级 `0.1.2` 并完成全量构建
  - Surfaced by: Release discipline — 桌面客户端必须显示可核对版本。
  - Files: `VERSION`, workspace manifests, desktop/PWA package manifests, bundle scripts
  - Verify: 版本同步、Rust workspace、PWA、desktop shell 和 DMG 构建。

- [ ] **T9 (P1, human: ~30min / Codex: assisted)** — Real-device QA — 完成 Desktop、LAN、Quick Tunnel 真机验证
  - Surfaced by: Test Decision A — 自动测试不能替代真实 host bridge 的 cwd 语义。
  - Files: `docs/dogfood-runs/`
  - Verify: 真实项目创建、cwd 一致、篡改拒绝、目录失效恢复。

## 评审完成摘要

- Step 0 Scope Challenge：范围收敛为复用现有 Codex session cwd，不建设完整目录授权系统。
- Architecture Review：3 个问题，全部已有明确决策。
- Code Quality Review：3 个问题，全部已有明确决策。
- Test Review：已生成覆盖图，识别 30 条待实现测试路径。
- Performance Review：0 个新增问题。
- NOT in scope：已记录 6 项。
- What already exists：已记录并优先复用现有链路。
- TODOS.md：0 项；没有值得脱离本次版本单独延期的工作。
- Failure modes：0 个无测试、无处理且静默失败的关键缺口。
- Outside voice：本次未运行，不阻塞实施。
- Parallelization：3 条逻辑 lane；考虑脏工作区和共享协议，实际顺序实施。
- Lake Score：7/7 个工程决策选择完整方案。

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|---|---|---|---:|---|---|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 0 | - | 本功能沿用已确认产品范围 |
| Codex Review | `/codex review` | Independent 2nd opinion | 0 | - | 未运行 |
| Eng Review | `/plan-eng-review` | Architecture & tests | 1 | CLEAR | 6 个工程问题、30 条测试路径、0 个关键缺口 |
| Design Review | `/plan-design-review` | UI/UX gaps | 0 | - | 已有经用户确认的新建会话原型 |
| DX Review | `/plan-devex-review` | Developer experience | 0 | - | 不适用 |

**VERDICT:** ENG CLEARED，进入实现。

NO UNRESOLVED DECISIONS
