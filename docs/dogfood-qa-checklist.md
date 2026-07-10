# Codex Mobile Bridge 内部试用 QA Checklist

本文档用于公司内部同事试用 MVP。目标不是证明所有功能已经产品化，而是用同一套步骤判断“普通 Codex API 用户是否能离开电脑后继续处理任务”。

## 试用前提

- macOS 电脑已安装新版 ChatGPT Desktop 或仍叫 Codex 的旧安装，并已用 API 方式登录可正常执行任务。
- 手机和电脑在同一可信网络下；外网访问只使用明确标注为 Beta 的临时链接。
- 试用期间不要把本机端口、pairing URL、control token、诊断日志直接公开到群聊或公网。
- 配对链接是一次性入口；如果页面提示失效，重新从电脑端生成新链接。

## 记录信息

- 试用日期：
- macOS 版本：
- ChatGPT/Codex Desktop 版本：
- Codex Mobile Bridge commit：
- 手机型号和浏览器：
- 网络环境：局域网 / 手机热点 / Quick Tunnel Beta

## 安装与首次启动

- [ ] 下载或本地构建当前版本。
- [ ] 启动桌面侧 Bridge。
- [ ] 首屏能看到 Bridge 状态，而不是空白窗口或只有终端日志。
- [ ] Bridge 能显示本机局域网访问地址。
- [ ] Bridge 能显示“生成手机配对链接/二维码”的入口。
- [ ] 首次启动不要求用户手动复制 control token。
- [ ] 如果 ChatGPT/Codex Desktop 未安装，界面给出安装和打开桌面应用的明确指引。

通过标准：用户不需要懂 CDP、remote debugging port 或 sidecar，就能知道下一步该做什么。

## ChatGPT/Codex Desktop 附着

- [ ] ChatGPT/Codex Desktop 未运行时，Bridge 可以打开对应桌面应用，或提示用户点击启动。
- [ ] ChatGPT/Codex Desktop 已运行且 CDP 可用时，Bridge 显示 `writable` 或等价可回写状态。
- [ ] ChatGPT/Codex Desktop 已运行但没有 debug port 时，Bridge 不自动杀进程、不静默重启，必须提示需要用户确认重启。
- [ ] 如果机器上只有 `ChatGPT Classic`，Bridge 不应把它当成可用的 Codex-capable 目标。
- [ ] 降级状态显示具体原因：`cdp_unavailable`、`target_not_found`、`inject_failed`、`rpc_unavailable`、`read_only`。

通过标准：用户知道是“ChatGPT/Codex 没启动”、“需要重启桌面应用”，还是“当前桌面版本注入失败”。

## 手机配对

- [ ] 手机扫码或打开配对链接后能完成首次配对。
- [ ] 配对完成后刷新页面仍保持登录。
- [ ] 使用过的 pairing token 再次打开不会获得新 session。
- [ ] 过期 pairing token 显示需要新链接，而不是无限加载。
- [ ] 同一手机关闭浏览器再打开，能使用已保存凭证继续连接。

通过标准：一次扫码形成长期设备绑定；失效链接的处理可理解。

## 会话读取

- [ ] 手机端能看到最近 ChatGPT/Codex 会话列表。
- [ ] 会话列表按 Codex 体验展示，最近更新的任务易于找到。
- [ ] 进入任一会话后，消息按时间从上到下展示，最新消息在底部。
- [ ] 用户消息和 Codex 回复有清晰区分。
- [ ] 图片附件通过 Bridge 代理显示；本机路径不直接暴露给手机。
- [ ] 消息时间使用真实事件时间，不出现默认纪元日期或明显时区错误。

通过标准：用户能在手机上判断当前任务上下文，而不是只看到零散事件。

## 文本回复

- [ ] 选择一个可回写会话。
- [ ] 在手机输入文本并发送。
- [ ] ChatGPT/Codex Desktop 对应 thread 收到文本并继续执行。
- [ ] 手机端不会重复显示同一条用户消息。
- [ ] ChatGPT/Codex 生成回复后，手机端能在短轮询周期内看到更新。
- [ ] 只读降级时，输入框被禁用并说明原因。

通过标准：手机发送的是对真实 Codex thread 的回写，不只是前端本地新增消息。

## 审批处理

- [ ] 触发一个需要 approval 的 Codex 操作，或使用 debug approval 入口做 smoke test。
- [ ] 手机顶部只在有待处理审批时显示 Pending approvals。
- [ ] 审批卡片能展示操作标题、风险提示和来源会话。
- [ ] 点击 approve 后，桌面任务继续执行。
- [ ] 点击 reject 后，桌面任务收到拒绝结果。
- [ ] 已处理 approval 不会一直占据顶部空间。

通过标准：离开电脑时，手机可以解除“等待用户确认”的阻塞。

## 远程链接 Beta

- [ ] 用户明确点击启用远程链接后，才启动 tunnel。
- [ ] 页面或桌面端明确标注 Quick Tunnel / 临时公网链接为 Beta。
- [ ] tunnel 旋转后生成新 pairing link，旧链接不再被当作当前入口。
- [ ] tunnel 断开时，手机端显示连接错误和“需要新链接”，不显示为正常在线。
- [ ] 关闭远程链接后，公网 URL 不再可访问。

通过标准：公网能力是用户显式开启的临时 Beta，不被包装成稳定远程服务。

## 设备撤销

- [ ] 桌面端能列出已配对设备。
- [ ] 撤销某台设备后，该手机刷新或下一次请求显示 session 失效。
- [ ] 被撤销设备不能继续读取 sessions、events、assets 或发送消息。
- [ ] 重新配对需要新的 pairing token。

通过标准：丢手机或借手机测试后，用户能从电脑端收回访问权。

## 诊断导出

- [ ] 能导出诊断包。
- [ ] 诊断包包含 app version、sidecar version、Codex adapter 状态、Bridge 状态、Tunnel 状态和最近连接状态。
- [ ] 诊断包不会包含 control token、Authorization header、API key、pairing token 或本机完整路径。
- [ ] 用户仍应在发送诊断包前人工扫一眼内容。

通过标准：普通用户遇到问题时，可以给开发者提供足够线索，同时不泄漏敏感凭证。

## 失败报告模板

```text
问题一句话：
发生在哪一步：
期望结果：
实际结果：
是否局域网 / tunnel：
Bridge 状态：
Codex attach 状态：
手机浏览器：
是否可复现：
诊断包已检查并附上：是 / 否
```
