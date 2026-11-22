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
