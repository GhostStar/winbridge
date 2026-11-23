# Pointer ownership and boundaries / 鼠标归属与边界

## 中文

普通窗口操作时，只有事件所属的 Mac 窗口、当前可见画面范围和本地最前方命中窗口
都匹配，才转发鼠标移动、点击、滚轮及捏合缩放。标题栏、留白、隐藏／最小化窗口、
其他窗口的事件，以及落在本地遮挡窗口上的迟到事件不应移动 Windows 鼠标。
不需要开启“背景禁悬停”才能阻止越界；该选项仍只控制鼠标在非键盘焦点窗口内部的悬停。

根因：旧版 `mouseMoved` / `mouseDragged` 不验证画面边界，直接发送可能小于 0 或大于 1
的归一化坐标；主机会把这些坐标换算成桌面位置。`mouseExited` 只更新光标外观，未释放
拖动状态。失焦／隐藏时的按钮释放还会附带旧窗口坐标，可能把主机鼠标拉回旧位置。

修复行为：

- 普通鼠标移出画面或进入另一个窗口时，立即原地释放本窗口已经按下的远端鼠标按钮。
  复用现有 `is_relative` 按钮路径，只释放、不移动远端指针；无需更新 Windows。
- AppKit 继续送来的越界拖动／抬键，以及已结束拖动后移回的拖动事件均不转发。
  必须重新按下才能开始另一轮拖动；没有按下记录的抬键不会影响其他窗口。
- 即使坐标仍落在旧窗口的矩形中，也通过窗口服务器检查该位置当前真正属于哪个本地窗口。
- 归一化坐标在发送出口再次检查有限值和 0…1 范围。鼠标外移不取消键盘焦点或中文输入。
- 显式游戏锁定仍保留相对移动／限位功能，但本地事件拦截器只能处理自身键盘焦点窗口的
  事件，不能吞掉另一个窗口的点击。普通窗口不使用游戏锁定的越界行为。

验证使用两个独立 AppKit 合成窗口，覆盖四边越界、事件串窗、重叠遮挡、拖动中移出、
遗漏退出通知、迟到释放、移回不恢复拖动、留白、隐藏窗口及非法坐标。测试窗口短暂置前
并等待窗口服务器提交层级，结束后关闭；不点击用户聊天或移动真实鼠标。
还运行完整 Swift / Quinn 回环回归。实际 QQ／微信多窗口手感仍需用户更新后验收。

## English

In ordinary viewing mode, pointer input is forwarded only when the event belongs to the
viewer window, lands inside its visible video, and the window server identifies that viewer
as the frontmost mouse hit. Borders, letterboxing, hidden/minimized viewers, foreign-window
events and locally obscured stale events must not move the host pointer. This does not depend
on the optional inactive-hover setting, which still governs hovering inside an inactive viewer.

Previously, move/drag handlers sent unchecked normalized coordinates outside 0…1. The host
converted them into desktop positions. Exit only changed cursor appearance; it did not end a
drag. Focus-loss button releases also carried old absolute positions and could warp the pointer.

Leaving ends the remote mouse drag by releasing this viewer's held buttons in place through
the existing relative-button path. Trailing drags/ups cannot revive it, and unpaired button-ups
are discarded. Re-entering requires a new press before dragging again. Keyboard/IME focus is
preserved. The outbound path additionally rejects non-finite and out-of-range absolute positions.
Explicit game lock retains relative/confined behavior, but its local monitor cannot intercept
another window's events. No host/protocol change is required.

Native AppKit tests cover two-window routing, real window-server overlap, all four boundaries,
missing exit notifications, release without warping, re-entry, letterboxing, hidden viewers and
invalid coordinates. They create and close synthetic windows without clicking user content or
moving the physical pointer. Physical QQ/WeChat acceptance remains a user verification step.
