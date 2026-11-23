# Border resizing / 拖边框调整大小

## 中文

- **直接拖动 Mac 窗口边框或角落**：只等比缩放本地画面，不改变 Windows 窗口大小。
- **按住 Option（⌥）开始拖动边框**：调整该 Windows 源窗口的宽高，主窗口和带边框的
  附属弹窗使用相同操作。保持按住不是必需的：模式按开始拖动时的按键固定，松手结束。
- 下一次普通拖动自动回到本地缩放。没有全局开关，也没有每窗口模式选择器；旧版
  `syncWindowResize` 设置被忽略并在保存设置时移除。
- 不可调整尺寸、被模态窗口禁用、完整桌面等目标不进入主机调整，仍可本地缩放并提示原因。
- Mac 标题栏关闭、最小化、最大化和全屏始终只操作本地查看窗口。拖动远端画面内
  Windows 自带的边框仍是远端鼠标操作，不使用此快捷手势。

管理界面显示操作提示。Option 手势期间窗口标题显示“调整主机大小”，不保留模式。
不增加独立模式选择器或快捷键设置。

### 尺寸与更新

手势开始时固定“源窗口物理像素 / 当前画面 Mac 点数”的比例，排除 Mac 标题栏。
不再直接按 Windows DPI 乘本地尺寸，也不额外乘 Retina 倍率，因此此前本地缩放过的
窗口不会在第一次调整主机时突然跳回 100%。Windows 仍可约束实际结果。

普通拖动由 AppKit delegate 保持画面比例；Option 拖动允许宽高独立变化。主机调整时
仍保持视频等比显示，等待新尺寸画面的短暂期间可能出现留边。尺寸请求约 100 ms
合并发送，计时器加入原生事件追踪循环；松手立即发送最后尺寸并取消待发送定时器。

主窗口及附属窗口的拖动、待确认状态先于 owner 相对布局处理；期间旧快照不得覆盖
本地几何。收到最终修订确认后按开始时的比例校准主机实际尺寸，2 秒未确认则释放
几何保护。关闭、失去可调整能力、进入本地全屏／缩放动作或断线会取消手势。
拖边框开始时释放远端已按下的键鼠状态，手势期间不转发画面输入。

本次仅修改 Mac 客户端，沿用现有 ResizeWindow / ack_resize_seq，Windows 不需更新。
协议目前只传宽高，Windows 源窗口位置不随 Mac 左边／上边拖拽而改变。

### 验证

AppKit 合成窗口回归覆盖：普通拖动保持比例且无远端请求；Option 模式锁定、下次
默认本地缩放；非默认缩放比例换算；附属窗口拒绝旧几何并接受最终确认；不可调整／禁用
目标；关闭／全屏取消；事件追踪模式中持续发送、松手最终请求及无迟到请求。
同时运行真实 Quinn 回环互通与其余 Swift 回归。物理鼠标／触摸板拖边框和实际 QQ／微信
尺寸限制仍需用户验收，合成测试不代表这些应用已通过实测。

## English

Drag a Mac viewer border normally to scale the local view proportionally. Hold **Option (⌥)**
when starting the border drag to resize the Windows source window instead. This applies to
both independent viewers and titled owned panels. The choice is latched for that gesture;
releasing Option midway does not switch modes, and the next ordinary drag is local again.
The old global setting is retired. There is no persistent per-window mode selector.

Fixed-size, disabled and full-desktop targets stay in local scaling with an explanatory hint.
Mac titlebar close/minimize/zoom/fullscreen remain local, including with Option held. Dragging
the Windows border inside the remote image remains ordinary remote pointer input.

The gesture freezes source physical pixels per displayed Mac point, excluding the Mac titlebar.
Neither host DPI nor Retina introduces an extra multiplier. Source width/height are sent
independently, coalesced about every 100 ms through a common-mode run-loop timer, with an immediate
final request on release. Playback remains proportional; temporary margins may occur until
frames at the new aspect ratio arrive. Windows may constrain the actual dimensions.

Live/pending geometry protection precedes owner-relative panel layout. Final acknowledgement
reconciles actual dimensions using the frozen scale; protection expires after two seconds if
unacknowledged. Closing, capability loss, local fullscreen/zoom and disconnect cancel the gesture.
Held remote keys/buttons are released at gesture start and viewer input is suppressed during it.

Only the Mac client changes. Existing ResizeWindow/ack_resize_seq framing is reused; the host
position remains fixed because the protocol carries dimensions only. Tests exercise native
AppKit window/delegate behavior, tracking-mode timers, scale conversion, panel reconciliation,
capability gates and cancellation, alongside Quinn interoperability. Physical border gestures
and real QQ/WeChat resize constraints remain user acceptance checks.

AppKit supplies the initiating modifier flags through
[NSWindow.resizeFlags](https://developer.apple.com/documentation/appkit/nswindow/resizeflags)
and delegates proportional size decisions through
[windowWillResize](https://developer.apple.com/documentation/appkit/nswindowdelegate/windowwillresize(_:to:)).
