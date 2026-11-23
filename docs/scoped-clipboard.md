# Scoped clipboard / 按窗口授权的剪贴板

## 中文

此 fork 采用 Kokoda 的按窗口请求语义，替换 WinBridge 的全局剪贴板广播。
需要同时更新 Mac 客户端和 Windows 主机；不支持新能力的旧主机不会回退到旧同步。

- Mac 仅在当前投射窗口执行编辑菜单或 `⌘/Ctrl+C/X/V` 时发起文本或图片事务。连接、聚焦和剪贴板变化均不会自动传输。管理界面的“允许复制粘贴”是这些操作的总开关。
- 粘贴先校验、写入 Windows 剪贴板，释放剪贴板锁后再发送 Ctrl+V，避免先粘贴到旧内容。
- 复制/剪切等待剪贴板序号变化并验证来源。后台轮询已移除，主机拒绝旧 `poll` 请求。
- 在远端右键菜单复制后，主动选择 Mac“编辑 → 取回远端已复制内容”。此 `fetch` 操作不注入 Ctrl+C，
  读取仍须通过同样的窗口、前台与所有者校验；它明确读取该应用已有内容，不要求内容刚刚发生变化。
  不操作这些命令时，不读取连接前或当前剪贴板内容。
- 主机校验订阅、订阅代次、进程创建时间、精确前台窗口和可用状态。读取还需剪贴板所有者
  属于该投射应用组的已订阅进程。拒绝完整桌面目标、过期句柄、重放请求及旧全局写入消息。
- Mac 仅接受匹配的未过期请求；失焦、隐藏、关窗、断线或关闭同步后不应用旧结果。
  等待期间如果本地复制了新内容，则保留本地新内容。
- 文本限 1 MiB UTF-8，PNG 限 8 MiB，解码限 16 Mi 像素；无效内容在清空剪贴板前拒绝。
- 移除剪贴板文件枚举和自动取回；上传文件也不再改写 Windows 剪贴板。显式选择文件传输仍保留。
  文件浏览器中明确请求复制到 Mac 剪贴板的结果同样受同步开关、连接代次和本地变更保护。

### 已更新文件但仍无法复制粘贴

以实际连接握手的能力位为准，而不是下载包或显示的 `0.5.2` 版本号。旧主机可能也显示
同一版本号。能力值 `511` 缺少 bit 9，新版主机为 `1019`。如果新版文件已下载但握手仍
不支持，检查是否有旧后台进程、旧 Windows 服务或另一目录的可执行文件仍监听端口。
退出/停止旧实例，再启动新版并重新连接。客户端会在连接提示和当前投射窗口的提示条中
解释不兼容原因，不回退到全局剪贴板同步。

### 边界

Windows 系统剪贴板仍然是全局的：主动向远端粘贴会修改它，其他 Windows 应用仍可能读取。
这里限制的是网络读取/写入的来源、时机和目标，并非操作系统级应用隔离。剪贴板所有者检查
粒度是订阅进程组，不能区分同一进程里每个文档。窗口前台检查与系统键盘注入不是原子操作，
不要同时在主机上抢焦点。已开始的剪切/粘贴不会因为客户端之后失焦而被撤销。

某些应用把剪贴板交给未订阅的辅助进程或没有有效所有者，内容会被拒绝而不是扩大读取范围。
关闭允许复制粘贴时 `C/X/V` 剪贴板快捷键会被消费，不会退回到粘贴主机旧内容。普通输入不受影响。

### 验证

2026-11-22：Mac 62 项、Windows 53 项常规测试通过；另通过原生剪贴板事务（含独立进程辅助用例）和 4 项弹窗交互回归。

- Mac Swift 测试使用独立命名 NSPasteboard，不读取或改写用户系统剪贴板；覆盖文本、透明图片、
  迟到响应、本地内容变化、窗口代次、关闭同步、无关窗口、旧主机、文件发布及畸形数据。
- Windows 单元/回环测试验证旧全局消息、未订阅目标和 `clipboard:` 文件请求被拒绝。
- Parallels Windows 11 上运行 x64 原生交互测试：临时关闭宿主机/虚拟机剪贴板共享，使用合成
  EDIT 窗口验证粘贴→复制→剪切、畸形图片不清空剪贴板、订阅 token/请求重放拒绝；另起独立
  进程写入合成内容，确认不会被当前窗口的轮询取回。测试后清空合成内容并恢复共享设置。
- 原生测试仅在隔离测试桌面运行：`cargo test --test scoped_clipboard -- --ignored --test-threads=1`。
  这不是 QQ/微信实机兼容性测试，具体应用仍需验证。

## English

This fork replaces global clipboard broadcasting with Kokoda-style window-scoped
transactions. Upgrade both endpoints. Capability bit 9 is required; unsupported
hosts do not fall back to global sync. No local clipboard changes are exported in
the background and unsolicited legacy updates are ignored.

Copy/cut/paste carry a subscription generation and monotonic request ID. The host
validates the live subscription, process creation time, exact foreground target and
enabled state. Reads also require an owner in the subscribed capture group. Paste
publishes validated text/PNG before injecting Ctrl+V; copy/cut wait for a new
sequence. There is no background polling, and the host rejects legacy `poll` requests.
Use Edit → Get remote copied content after a remote context-menu copy. This explicit
`fetch` reads existing content from an eligible foreground owner without injecting Ctrl+C;
it does not require a recent clipboard change. Connecting, focusing and clipboard changes
alone never transfer content. Edit menu copy/cut/paste and keyboard shortcuts share one path. The observer services sent Windows messages while waiting so clipboard
ownership transfer does not deadlock after a paste. See Microsoft's
[PeekMessage documentation](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-peekmessagew)
and [WM_DESTROYCLIPBOARD](https://learn.microsoft.com/en-us/windows/win32/dataxchg/wm-destroyclipboard).

The client rejects stale/mismatched results after focus changes, hiding/closing,
disconnection or disabling sync. A newer local clipboard change wins. Limits are
1 MiB UTF-8 text, 8 MiB PNG and 16 Mi decoded pixels. Clipboard-file enumeration and
automatic downloads are removed; uploads no longer publish CF_HDROP. Explicit file
selection remains, with guarded clipboard publication only when requested.

This is not an OS-private clipboard: explicit paste changes the Windows system
clipboard and process-level ownership cannot distinguish documents in one process.
Foreground checks and SendInput are not atomic. Unrecognized helper owners fail
closed. Already started cut/paste actions cannot be undone on later focus loss.

Tests use isolated named pasteboards on Mac and synthetic content on Windows with
VM clipboard sharing disabled. They cover text/images, malformed payloads, stale
responses, generation/replay checks, explicit file publication, native paste/copy/
cut ordering and rejection of a separate clipboard-owner process. Native QQ/WeChat
behavior remains a manual compatibility check.

### Updated files but clipboard still unavailable

Check the connected host's negotiated capabilities, not just its download filename
or `0.5.2` version label. Mask `511` lacks bit 9; this fork's scoped-clipboard host
advertises `1019`. An old background process, Windows service or another executable
path may still own the listening port. Stop that instance, start the updated binary
and reconnect. Connection status and the active viewer now explain this mismatch;
global clipboard fallback remains disabled.

## Explicit-transfer revision / 显式传输修订

Upgrade both endpoints for manual retrieval and host-side rejection of old polling clients.
The wire keeps the retired `discard` field for compatibility, but a true value is rejected.
Requests still require authenticated subscriptions; request IDs and tokens do not prove a
physical human gesture. An authorized custom client can issue explicit actions, and a local
program with input automation permissions may simulate them. This change removes automatic
transfer triggers; it does not isolate the OS clipboard or defend a compromised endpoint.

手动取回及主机拒绝旧轮询客户端需要更新两端。协议保留旧 `discard` 字段，但拒绝 true。
请求仍须经过连接认证和窗口授权；请求编号及窗口令牌不证明真人操作。已授权的自定义客户端
可以发起显式命令，具备输入自动化权限的本机程序也可能模拟这些操作。本次改动移除自动传输
触发源，不提供系统剪贴板隔离，也不能防御已被控制的端点。

### Validation, 2026-11-24 / 本次验证

- Mac: 98 tests passed; 3 optional live/fixture tests skipped (101 discovered). Named test
  pasteboards cover idle/no-export, explicit fetch, replay/mismatch and reconnect rejection,
  menu/shortcut routing and existing text/image protections.
- Windows x64: 55 regular tests plus 1 isolated interactive clipboard test passed on Windows 11
  ARM under x64 emulation. Native coverage includes copy/cut/paste, explicit fetch, legacy poll
  rejection and no content returned for an unrelated owner process. VM clipboard sharing was
  disabled for the native run and restored afterwards. This is not a new QQ/WeChat hardware test.

Mac 共发现 101 项测试，98 项通过、3 项可选实机/素材测试跳过；新增空闲不传输、手动取回、
重放/不匹配/重连拒绝及菜单快捷键路由验证。Windows x64 常规 55 项通过，另 1 项隔离交互
剪贴板测试通过，覆盖复制/剪切/粘贴、手动取回、拒绝旧轮询以及无关进程内容不返回。
Windows 测试环境为 Windows 11 ARM 的 x64 模拟执行；原生测试期间关闭虚拟机剪贴板共享，
结束后恢复。本次未重新进行 QQ/微信物理机兼容性验证。
