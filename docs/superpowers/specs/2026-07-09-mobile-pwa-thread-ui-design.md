# Mobile PWA Thread UI Design

Date: 2026-07-09

## Context

Codex Mobile Bridge PWA 已经能通过 sidecar 连接 Codex Desktop，并能读取 sessions、thread events、发送消息。当前仍有三类体验问题：

- 左侧 Sessions 列表会跟随右侧长消息流一起滚动，切换会话不方便。
- 右侧消息顺序与 Codex Desktop 不一致；用户期望旧消息在上，新消息在下，新消息把内容往上顶。
- Codex raw event 中的 `localImage` 目前只作为文本路径出现，手机端不能直接显示图片。

本设计只覆盖 PWA thread UI 和本地图片显示能力，不改变 Codex Desktop 注入路径、配对协议、公网 tunnel 或审批策略。

## Goals

- Sessions 列表独立滚动，右侧消息流滚动时左侧保持可用。
- Thread event stream 按 Codex 风格展示：旧消息在上，新消息在下。
- 新消息到来时，如果用户在底部附近，自动保持底部；如果用户正在翻旧消息，不抢滚动。
- 能在消息中显示 Codex raw event 附带的本机图片缩略图。
- 保持 MVP 简洁，不引入完整附件库、上传系统或复杂权限策略。

## Non-Goals

- 不做通用文件下载中心。
- 不做图片持久缓存、缩略图预生成或云端同步。
- 不做跨设备共享附件。
- 不改配对 URL 的一次性 token 语义。
- 不实现 Codex Desktop 原生事件流订阅；短轮询仍是当前同步方式。

## Design

### Layout

PWA 主体保持三层结构：顶部连接状态栏、中间 workbench、底部 composer。中间 workbench 固定在可视高度内，内部由左侧 Sessions panel 和右侧 Session Detail panel 组成。

Sessions panel 自己滚动，右侧 event stream 自己滚动。左侧 panel 的 heading 和列表区域保留在同一个卡片内，列表高度由 workbench 剩余空间决定。右侧长消息滚动不会影响左侧会话选择。

### Event Ordering

所有展示事件统一按 `createdAt` 从小到大排序，即旧消息在上，新消息在下。

事件来源有三类：

- HTTP polling 返回的 canonical `listSessionEvents` 结果。
- WebSocket 收到的增量事件。
- 本地 optimistic user message。

HTTP polling 结果优先级最高，代表当前 thread 的 canonical snapshot。合并时：

1. 以 polling 结果为基准。
2. 保留尚未出现在 polling 结果里的本地或 WebSocket 事件。
3. 本地 pending user message 与服务端 user echo 用 trim 后文本匹配；匹配成功则移除 pending 版本。
4. 最终统一按 `createdAt` 升序排序。

### Scroll Behavior

Session Detail 维护 event stream 的滚动容器引用。

- 切换 thread 后默认滚到底部。
- 新事件合并后，如果用户距离底部小于阈值，例如 80px，则自动滚到底部。
- 如果用户正在查看历史消息，新增事件不强制跳到底部。

后续可以增加 “new messages” 小提示，但本次不做。

### Image Attachments

Codex raw event 可能包含形如：

```json
{
  "type": "localImage",
  "path": "/var/folders/.../codex-clipboard.png",
  "detail": null
}
```

后端 normalizer 在提取文本时同时识别图片附件，并将其放入 event payload，例如：

```json
{
  "role": "user",
  "text": "...",
  "attachments": [
    {
      "type": "image",
      "src": "/api/assets/local-image?token=...",
      "name": "codex-clipboard.png"
    }
  ]
}
```

图片不能直接暴露 Mac 本地路径给手机浏览器。sidecar 提供受鉴权保护的本地图片代理接口。接口只允许读取 normalizer 已登记的图片路径，避免任意路径读取。

前端 event row 渲染时先显示文本，再显示图片缩略图。图片加载失败时显示文件名和失败状态，不阻塞文本消息展示。

## Error Handling

- 图片路径不存在：返回 404，前端显示 “Image unavailable”。
- 非图片 MIME 或不可读文件：返回 415 或 403，前端显示附件占位。
- session 未授权：图片接口返回 401，与其他 API 保持一致。
- polling 失败：保留当前事件，不清空列表，并沿用现有 connection error 展示。

## Testing

Frontend tests:

- Sessions panel 与 event stream 使用独立滚动容器。
- polling events 合并后按旧到新展示。
- pending user message 与带换行的服务端回显不会重复。
- 切换 thread 后默认滚到底部。
- 用户不在底部时新事件不会强制抢滚动。
- 带 image attachment 的 event row 渲染缩略图。

Backend tests:

- normalizer 能从 raw `localImage` 提取 image attachment。
- 图片代理只允许已登记路径。
- 不存在、非图片、未授权请求返回明确错误。

## Acceptance Criteria

- 左侧 Sessions 在右侧消息流滚动时保持位置和可点击性。
- 右侧消息顺序与 Codex Desktop 一致：旧在上，新在下。
- 新消息持续到来时，位于底部的用户能看到消息自然向上顶。
- 刷新页面前后消息顺序一致。
- 包含本机图片的用户消息能在手机浏览器中显示图片缩略图。
- 所有新增测试通过。
