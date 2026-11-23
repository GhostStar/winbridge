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


## Hidden Windows popups / Windows 上仍打开的弹窗

A Mac close still hides only the local viewer. The Windows popup may remain above
its owner. The host previously treated any foreground window from the same PID
as success and injected global mouse input at screen coordinates without checking
which HWND received it. It now focuses the exact target (or a genuine child),
checks the point against that HWND/its child controls, and rejects clicks and
wheel input on other top-level surfaces. Rejected hover is silent; release
cleanup remains possible without warping. An already-held left drag retains its
existing anchor. Game-relative and secure-desktop paths retain their own behavior.
Keyboard/text input also requires successful target activation instead of sending
into whichever application happens to be foreground.

`HostError` uses `TARGET_OBSCURED blocker=<decimal HWND>` for this rejection, with
no title or process details. The Mac only restores a blocker already in its live
controller map and locally hidden, never creates a new subscription from this ID.
The failed input is not replayed. A visible message explains the popup is still
open on Windows. Unknown/untracked blockers show a refusal without revealing or
controlling them. Both endpoints must be updated for the complete behavior.

Mac 关闭仍只隐藏本地查看窗口，Windows 弹窗可能继续挡住主窗口。旧逻辑把同一
进程的其他前台窗口也当作聚焦成功，并直接按屏幕坐标注入。现在聚焦必须命中准确
目标或真正子控件；点击／滚轮遇到其他顶层窗口则拒绝，悬停不刷错误，释放按键
仍可原地完成。已有左键拖动保留锚点；游戏相对输入和安全桌面保持独立路径。
键盘／文字也必须成功激活目标后才能注入。

拒绝时仅返回遮挡 HWND，不包含标题或进程详情。Mac 只恢复已订阅、已有控制器且
本地隐藏的对应窗口，不因错误中新 ID 扩大订阅，不重放被拦截的输入；提示弹窗仍在
Windows 打开。未知遮挡只提示拒绝，不显示或控制该窗口。完整修复需同时更新两端。

Validation uses synthetic windows on the interactive Windows test desktop: an
owned same-process popup receives zero clicks/wheel events, the rejected pointer
does not move, and a real child control remains clickable after hiding the popup.
No private application was clicked. Mac regression covers error identity parsing
and retains local-close/no-remote-close checks. Actual QQ/WeChat remains acceptance
testing; this does not claim atomic exclusion against every desktop race.

验证使用 Windows 交互测试桌面的合成主窗口、同进程附属弹窗和子控件：遮挡弹窗
收到的点击／滚轮为零、拒绝时鼠标不移动、隐藏弹窗后正常子控件仍可点击；没有
点击私人应用。Mac 回归验证错误身份解析，并保留本地关闭不远端关闭的检查。
真实 QQ／微信仍需用户验收。
