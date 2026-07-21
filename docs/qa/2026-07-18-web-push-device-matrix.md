# Web Push 真机 QA 矩阵

此矩阵是 `v0.1.7 Beta` 固定域名锁屏提醒的发布记录。自动测试通过不等于真机通过；每一行必须填写真实设备、日期、测试人和结果，stable 发布前 iPhone 与 Android 必须全部完成。

## 环境信息

- Bridge 版本 / commit：
- macOS / ChatGPT Desktop 版本：
- Named Tunnel 固定域名：`https://codex.example.com`
- 测试人：
- 测试日期：

## 设备与四类通知

| 设备 | OS | 浏览器 / 形态 | Origin | 完成 | 等待审批 | 等待输入 | 错误 | 点击 deep-link | 声音 | 震动 | 结果 | 日期 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| iPhone（当前支持版本） |  | 添加到主屏幕的 PWA | 固定 HTTPS |  |  |  |  |  | 系统控制 | 系统控制 | 待测 |  |
| Android |  | Chrome 浏览器页 | 固定 HTTPS |  |  |  |  |  | 系统通知 | 分类型 pattern（支持时） | 待测 |  |
| Android |  | 安装 PWA | 固定 HTTPS |  |  |  |  |  | 系统通知 | 分类型 pattern（支持时） | 待测 |  |

## 生命周期与故障恢复

| 场景 | 预期 | 结果 | 证据 / 备注 | 日期 |
| --- | --- | --- | --- | --- |
| iPhone Safari 普通标签页 | 显示添加主屏幕指引，不请求 permission | 待测 |  |  |
| permission denied 后再次启用 | 不重复弹权限请求，显示 Blocked | 待测 |  |  |
| 浏览器 subscription 丢失 | 显示 Needs repair，Repair 后恢复 | 待测 |  |  |
| 系统设置撤销通知权限 | 显示 Blocked/Needs repair，不误报 Active | 待测 |  |  |
| Bridge 重启 | pending outbox 恢复，不重复 AlertEvent | 待测 |  |  |
| Mac 切换 Wi-Fi | Named Tunnel 恢复后新提醒可达，不补发已终止旧事件 | 待测 |  |  |
| Named Tunnel 短断网 | 有限重试，总发送最多 4 次 | 待测 |  |  |
| Push 服务返回 404/410 | subscription 失效并进入 Needs repair，不持续重试 | 待测 |  |  |
| 同一 eventId 经 WS + Push 同时抵达 | 前台声音/震动只发生一次 | 待测 |  |  |
| 页面可见时普通 push | 不弹系统通知，只 postMessage 给页面 | 待测 |  |  |
| Settings 中发送测试通知 | 页面可见时仍弹一条 force system 通知 | 待测 |  |  |
| 点击后台通知 | 聚焦/打开 PWA 并选择目标 thread | 待测 |  |  |
| 点击已删除会话通知 | 保留当前会话并提示“Session is no longer available” | 待测 |  |  |
| 撤销设备 | 设备 session 失效，subscription/outbox 清理，不再收到通知 | 待测 |  |  |
| Named 切换 Quick/Local | pending/retry outbox 终止，不在恢复 Named 后补发 | 待测 |  |  |
| Quick Tunnel | 只显示前台提醒限制，不请求 permission、不创建 subscription | 待测 |  |  |

## 发布结论

- [ ] iPhone 固定域名锁屏四类通知均在 15 秒内到达。
- [ ] Android Chrome 浏览器页和安装 PWA 均完成。
- [ ] denied、Repair、404/410、重启、断网、换 Wi-Fi、设备撤销均完成。
- [ ] Quick Tunnel 降级行为准确。
- [ ] 可设置 `PUSH_QA_IOS_ACK=true`。
- [ ] 可设置 `PUSH_QA_ANDROID_ACK=true`。

最终结论：待真机 QA。
