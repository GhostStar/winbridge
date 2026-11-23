# Trackpad scrolling / 触摸板滚动

## 中文

QQ 实测反馈为聊天记录向上可滚动、向下困难，好友列表双向困难。源码对比发现原 WinBridge
将触摸板增量乘 3，Kokoda 乘 12。WinBridge 主机只累计小于 1 个滚轮单位的小数部分，
并没有累计到标准 120 单位刻度；两者也都不能保证目标控件会正确累计细小增量。
此现象与小增量兼容问题一致，但没有在用户 QQ 上抓取事件，尚不能确认唯一根因。

本次 Mac 客户端默认开启“触摸板兼容滚动”：

- 采用与 Kokoda 相同的 12 倍触摸板换算，每轴累计后发送 ±120 的整数倍。
- 上下左右对称处理；反向时清除前一方向尚未发送的余量。
- 新手势、取消、失焦释放和离开视图时清除余量；不同窗口各自累计。
- 惯性滚动延续同一手势；普通鼠标滚轮不受兼容累积影响。
- “窗口”菜单或菜单栏状态菜单可关闭兼容模式，恢复小增量平滑输出（12 倍）。
  设置会保存，已打开窗口立即使用，无需重连。

只需更新 Mac 客户端，现有 Windows 主机和协议不变。此改动针对滚动增量；没有顺带修改
窗口激活、遮挡检查或权限逻辑。兼容模式是离散滚轮刻度，平滑程度可能低于平滑模式。

验证：66 项 Mac 自动化测试通过，包括模拟“每次消息只接受完整刻度”的控件、连续小幅
双向滚动、反向余量、新手势、窗口隔离、模式切换、横向方向、普通鼠标和异常输入限制。
需要在 QQ 聊天记录和好友列表中复测上下滑动；尚未宣称 QQ 实机已修复。

## English

The reported QQ behavior is asymmetric scrolling in chat history and poor scrolling
in both directions in the contacts view. WinBridge used a 3x trackpad multiplier;
Kokoda uses 12x. The existing host accumulates fractional wheel units, not whole
120-unit detents. A control that discards each small delta can therefore fail to
scroll. This is a compatibility hypothesis, not a confirmed QQ event trace.

The Mac client now defaults to compatible trackpad scrolling: 12x scaling and
per-view, per-axis accumulation into complete 120-unit detents. Reversals discard
the previous direction's remainder. Gesture starts, cancellation, focus release
and pointer exit reset it; momentum continues the current gesture. Physical mouse
notches remain unchanged. Disable “Compatible trackpad scrolling” in the Window or
status menu for smooth small deltas. Changes apply live and persist.

No host/protocol update is required. This change does not modify focus or hit-test
policy. Compatibility mode trades continuous scrolling for standard wheel steps.
The 66-test Mac suite passes, including a simulated detent-only consumer, reversals,
window/mode isolation and physical mouse behavior. Physical QQ verification remains
outstanding.

Windows supports fractional detents only when applications accumulate them or
scroll partial lines: [WM_MOUSEWHEEL documentation](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-mousewheel).
