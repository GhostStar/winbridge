# Popup and transparency validation / 弹窗与透明度验证

## Scope

This fork adds ownerless application-viewer tracking and an optional atomic alpha
plane. It does not use titles such as “image” or “photo”, nor hard-coded QQ/WeChat
class names. Process-name matching alone is insufficient. A new foreground viewer
requires one unambiguous tracked root and a process instance belonging to that
application. Invisible owner-chain intermediates are queried locally and never
become extra projected windows.

测试仅操作隔离窗口，没有点击私人聊天、读取私人图片或安装系统服务。具体 QQ／微信
版本及用户物理显卡仍需使用新包验收，不能把合成窗口测试等同于这些应用的实测。

## Verification

- Apple Silicon macOS: Swift suite, native focusable viewer/chrome, malformed alpha
  envelopes, dimension bounds, transparent/half-transparent/opaque-black pixels,
  premultiplied edges without double darkening, and clearing a previous mask.
- Windows 11 ARM Parallels, x64 executable: Rust unit and loopback tests; actual WGC
  of an ownerless, titled layered viewer created after subscribing to a chat fixture;
  independent NORMAL presentation, hide/reopen, RAW BGRA and H.264 color with per-frame
  alpha, and an old client without the capability receiving ordinary framing.
- The VM used **software Media Foundation H.264**. This does not certify hardware
  HEVC/NVENC/QSV/AMF performance. The GPU capture/readback path was exercised.
- A synthetic Windows H.264+alpha frame was decoded on the Mac using VideoToolbox;
  every alpha byte matched, opaque black remained opaque, and half-transparent
  white remained premultiplied at approximately 128 rather than darkening to 64.
- Unit coverage includes hidden/multihop owners, ordinary new viewers, disabled
  tracking, existing windows, ambiguous roots, helper ancestry, parent PID reuse,
  independent sibling processes, mask cache/resize, encoder-output timestamps,
  and atomic color/mask queue drops.

Windows interactive checks (only in an isolated, unlocked test desktop):

```powershell
cd win-host
cargo test --locked
cargo test --test app_popups -- --ignored --test-threads=1
```

`WINBRIDGE_TEST_FRAME_OUT` optionally exports one small **synthetic** H.264/alpha
sample. Set the same variable to that file on the Mac and run
`mac-client/scripts/run_tests.sh --disable-sandbox` for the cross-platform decode
check. Without this variable the fixture-dependent Swift case is skipped.

两端必须更新才能协商透明遮罩。主窗口使用“单窗”采集并勾选“追踪弹窗”；先投射聊天
窗口，再打开图片。图片查看器有原生 Mac 标题栏；资料卡和菜单保留浮层行为。已有窗口、
无法验证进程归属或多个根窗口无法判定归属的情况保留手动选择，不自动扩大投射范围。

Transparent capture adds staging readback and masked-frame CPU composition. Normal
opaque roots keep the existing GPU/IOSurface path. This round does not benchmark
large transparent videos, validate QUIC end-to-end, or certify every third-party window toolkit.

The subsequent window-management revision follows Kokoda semantics: independent
viewers, owned focusable panels, local-only titlebar close/zoom, subtree hide/restore,
late-update suppression, and modal disabled-state checks. Tests cover native panel
ownership and Window-menu exclusion, pointer release, disabled-parent local close,
metadata round trips, cyclic ownership bounds, and independent-viewer survival after
the capture root disappears. Explicit disconnect clears hidden state; hidden frames
continue decoding.

窗口管理回归使用合成窗口，未对真实 QQ／微信账号做操作。关闭／恢复、窗口菜单排除、
输入释放、主机模态阻断以及根窗口消失后的独立成员保留分别验证；隐藏流量暂停及重连
保留偏好暂未实现。

## 2026-11-23: owned-panel layout isolation / 附属弹窗布局隔离

A WeChat sticker panel was enlarged and placed over the parent title/HUD. The Mac
layout key excluded only POPUP windows, so DIALOG panels shared `proc:<process>`
with their main window. Applying that saved frame overwrote the correct initial
owner-relative geometry. Owned panels could also overwrite the saved main layout.

Only NORMAL/legacy UNSPECIFIED viewers now persist/restore independent layouts or
enter workspace presets. Both DIALOG and POPUP remain owner-relative and cannot
consume a saved process layout. Owned panels have no separate HUD. A panel size or
DPI update also recomputes parent-relative geometry, even when its offset is stable;
independent-window aspect/size reconciliation no longer overwrites that result.
Related normal document/image viewers remain independent. Local panel dragging,
resizing and close semantics remain available; no window is closed on Windows.

微信表情窗放大且覆盖父窗口标题/HUD的原因是：DIALOG 附属面板与主窗口使用同一进程布局键，
正确的初始相对位置随后被主窗口保存的尺寸和位置覆盖。现在附属面板既不读取也不写入独立
窗口布局，不进入工作区预设；其尺寸/位置按父窗口画面比例处理，且不重复显示性能 HUD。
面板自身尺寸或 DPI 变化时，即使相对偏移未变，也会重新计算。普通独立图片/文档窗口仍保留
独立窗口行为，附属面板的本地拖动、缩放和关闭语义保留。

Validation: 68 Mac tests pass, including native AppKit regression for a saved main
frame applied to a same-process panel, panel snapshot rejection, parent-relative
size updates and independent viewer layout eligibility. The bundle also includes
clipboard capability-mismatch messages in the active viewer and connection banner.
Physical WeChat confirmation is pending. No Windows host/protocol update is needed.

验证：68 项 Mac 测试通过，包含真实 AppKit 窗口几何回归。同包保留滚动修复，并补充剪贴板
能力不匹配的可见提示。微信实机效果待复测；本次无需更新 Windows 主机。


## 2026-11-24: preserve native borderless cards / 资料卡保留无边框

Previously every DIALOG received a Mac title bar, including lightweight native
cards. Field 23 now carries the host's conservative native-style classification.
The card remains an owned, focusable panel; it is not converted to a nonactivating
menu. Metadata changes update chrome in place without moving the captured content
or issuing remote resize/close commands. Transparency remains independent.

此前所有 DIALOG 都加上 Mac 标题栏，导致原本无边框的资料卡多出一排按钮。现在由
Windows 原生样式识别轻量浮层，经字段 23 传给 Mac；保留附属关系及焦点能力，
不降级为不可激活菜单。动态更新边框保持内容位置，不发送远端缩放／关闭操作。
透明度与是否加边框独立处理。旧主机保持旧行为，需要两端更新。

Regression coverage includes protocol defaults/round trips, opaque and transparent
focusable cards, owner-relative placement without title-bar height, live metadata
changes, modal fallback, and independent viewers retaining local buttons. A native
Windows fixture checks actual HWND styles, owner enabled state and same-HWND style
updates. Only synthetic windows are used; physical QQ/WeChat acceptance is pending.

回归覆盖协议兼容、透明及不透明资料卡的焦点／定位、动态边框变化、模态回退和独立
窗口按钮。Windows 原生夹具检查真实 HWND 样式、拥有者禁用状态及同一 HWND 的样式
更新。测试使用隔离合成窗口，真实 QQ／微信版本仍需用户验收。

Validation: 92 Mac tests passed, including the real Quinn handshake fixture;
Windows x64 passed 54 unit/loopback tests plus the interactive native-card test
on Windows 11 ARM under x64 emulation. Other ignored interactive tests were not
rerun for this presentation-only change. The release binary is PE32+ x86-64.

验证：Mac 92 项测试通过，含真实 Quinn 握手；Windows x64 的 54 项单元／回环测试
及 1 项交互式原生资料卡测试通过（Windows 11 ARM，x64 仿真执行）。本次仅修改
显示元数据，没有重跑其他标为忽略的交互测试。交付主机为 PE32+ x86-64。
