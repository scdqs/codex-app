# Mobile Session Drawer Design

Date: 2026-07-09

## Context

Codex Mobile Bridge PWA 当前在桌面宽屏下使用 Sessions 列表和 Session Detail 双栏布局。这个布局在桌面上切换会话方便，但在手机窄屏上会把 Sessions 面板堆到当前会话详情上方，导致首屏大量空间被会话列表占据。用户希望手机端把 Sessions 搬到左上角侧滑抽屉里，默认优先展示当前会话内容。

本设计只覆盖 PWA 的移动端布局与抽屉交互。桌面端继续保留现有左右双栏，不改变 sidecar API、会话同步、approval 协议、图片代理或消息排序逻辑。

## Goals

- 手机窄屏默认不展示 Sessions 面板，首屏优先显示 approvals、当前会话详情、消息流和 composer。
- 在左上角提供明确的 Sessions 入口，点开后从左侧滑出会话列表。
- Header 中连接状态和辅助状态全部向右对齐，为左侧抽屉按钮留出稳定位置。
- 打开抽屉、关闭抽屉、选择会话后自动关闭抽屉，交互符合手机端预期。
- 桌面端布局和现有独立滚动行为保持不变。

## Non-Goals

- 不重做桌面端信息架构。
- 不把 approvals 移入抽屉。
- 不新增会话搜索、分组、置顶或批量管理。
- 不改变 session event 的拉取、合并、排序、滚动策略。
- 不引入路由级页面切换；抽屉只是当前页面内的响应式 UI。

## Design

### Responsive Boundary

移动端断点沿用现有 `max-width: 720px`。在该断点内：

- `SessionList` 从主布局中移出，作为 drawer 内容渲染。
- `session-grid` 变成单列，只保留 `SessionDetail`。
- 主 workbench 仍保留顶部 `ApprovalQueue`，因为 approval 是高优先级状态，不应藏进会话抽屉。

在大于 720px 的视口内，保留当前双栏：左侧 Sessions，右侧 Session Detail。

### Header

Header 在移动端改成三列：

1. 左侧：icon-only Sessions 按钮，使用菜单图标，`aria-label="Open sessions"`。
2. 中间：`LAN bridge` 与 `Codex Mobile` 标识，允许压缩省略。
3. 右侧：连接状态 chip 与 secondary status chip，右对齐换行。

连接失败时，错误 detail 仍显示在品牌下方，但不能把右侧状态 chips 挤出屏幕。header 高度可以略增，但应明显小于当前截图中的纵向占用。

### Drawer

移动端抽屉从左侧滑出，宽度为 `min(84vw, 340px)`。抽屉包含：

- 顶部栏：标题 `Sessions`、数量、关闭按钮。
- 当前 `SessionList` 内容，沿用现有 session row 视觉密度和选中态。
- 空状态沿用现有文案。

抽屉打开时：

- 主内容覆盖半透明遮罩。
- 点击遮罩或关闭按钮关闭。
- 选择某个 session 后调用现有 `setSelectedThreadId`，随后关闭抽屉。
- composer 不参与重新排版，视觉上被遮罩覆盖即可。

### State And Component Shape

`App` 持有 `isSessionDrawerOpen` 状态。推荐拆成两个小组件边界：

- `ConnectionBar` 增加可选 props：`showSessionMenuButton`、`onOpenSessions`。
- 新增 `SessionDrawer`，接收 `open`、`sessions`、`selectedThreadId`、`onSelect`、`onClose`。

`SessionList` 继续作为纯展示组件复用，不在内部关心自己位于 panel 还是 drawer。移动端 drawer 可以复用同一个 `SessionList`，但外层 class 应区分 panel 和 drawer，避免桌面 CSS 被移动端抽屉规则污染。

### Accessibility

- 抽屉容器使用 `role="dialog"` 和 `aria-modal="true"`。
- 打开按钮使用 icon-only button，并提供明确 label。
- 关闭按钮提供 `aria-label="Close sessions"`。
- 抽屉打开时按 `Escape` 关闭。
- 抽屉打开后初始焦点落到关闭按钮或当前选中 session；关闭后焦点回到打开按钮。

第一版不强制实现完整 focus trap，但不能让键盘用户无法关闭抽屉。

## Error Handling

- 如果 Sessions 为空，抽屉中显示现有 empty state。
- 如果连接状态异常，抽屉按钮仍可点击，用来查看缓存的会话列表或空状态。
- 如果切换 session 后事件加载失败，沿用现有 connection error，不在 drawer 内新增错误层。

## Testing

Frontend unit/CSS tests:

- 移动端 CSS 中 `session-grid` 不再给 Sessions 预留顶部行高。
- 移动端存在抽屉相关 class，并且桌面端保留双栏 grid。
- `ConnectionBar` 在移动端支持左侧 session menu button，状态 chips 仍右对齐。
- 点击 Sessions 按钮打开 drawer。
- 点击遮罩或关闭按钮关闭 drawer。
- 点击 drawer 内 session 后关闭 drawer，并更新 selected thread。
- 桌面端仍渲染常驻 Sessions panel。

Rendered browser checks:

- 手机宽度下默认首屏不出现常驻 Sessions 面板。
- 手机宽度下左上角按钮可打开抽屉，抽屉覆盖主内容但不破坏 composer。
- 桌面宽度下仍是 Sessions + Detail 双栏。
- 控制台无相关错误。

## Acceptance Criteria

- 在手机视口中，默认页面顶部不再展示完整 Sessions 列表。
- 用户可以从左上角打开 Sessions 抽屉并切换会话。
- 切换会话后抽屉自动关闭，当前会话详情更新。
- Header 状态信息靠右排列，不与抽屉按钮或品牌文字重叠。
- 桌面端现有双栏布局不退化。
- 所有新增和既有前端测试通过，PWA 构建通过。
