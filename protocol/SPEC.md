# WinBridge Protocol Specification (v0.2)

WinBridge is a LAN-first seamless remote window protocol allowing a **macOS Client** (Swift) to display and control individual windows hosted on a **Windows Host** (Rust).

Design influences:
- **Xpra**: Normal vs. Override-Redirect (`POPUP`) window model and cursor shape sync.
- **Microsoft `[MS-RDPERP]` (RAIL)**: Visible frame bounds (`DWMWA_EXTENDED_FRAME_BOUNDS`), owner/modal relationships, and sequence-numbered resize anti-looping.
- **RustDesk / Sunshine**: Low-latency HEVC/H.264 Annex-B streaming, Opus audio, TLS TOFU pinning, and clean input abstraction.

---

## 1. Transport Layer (Single Port, TLS 1.3 + Dual Channels)

Windows Host listens on a single TCP port (default: **`9876`**) so only one Windows Firewall rule is required. Both connections **MUST** set `TCP_NODELAY = true` and wrap the TCP stream in **TLS 1.3** (with TLS 1.2 minimum fallback; `--no-tls` opt-out for local debugging only). Clients authenticate the host certificate via **TOFU (Trust On First Use)** SHA-256 certificate fingerprint pinning per `host:port`, and pin the Media connection to the exact leaf certificate verified on the Control connection.

LAN discovery uses mDNS / Bonjour service type **`_winbridge._tcp.local.`** with TXT keys `v=1`, `tls=1|0`, and `fp=<8-byte hex prefix>`.

Every incoming TCP connection starts with an **8-byte ASCII+version preface** so the host can route it:

| Channel | Direction | 8-Byte Preface (Hex) | Purpose |
| :--- | :--- | :--- | :--- |
| **Control** | Bidirectional | `WBRGCTL\x01` (`57 42 52 47 43 54 4C 01`) | Handshake, window metadata, resize, input, cursor, clipboard |
| **Media** | Win → Mac | `WBRGMED\x01` (`57 42 52 47 4D 45 44 01`) | Multiplexed raw video frames for all subscribed windows |

### Connection Handshake Flow

```mermaid
sequenceDiagram
    participant Mac as macOS Client
    participant Win as Windows Host (:9876)

    Note over Mac,Win: 1. Open Control TCP Connection
    Mac->>Win: 8B preface "WBRGCTL\x01"
    Mac->>Win: ControlMessage::Hello (auth_token, supported_codecs, display_scale)
    Win->>Mac: ControlMessage::HelloAck (accepted=true, session_id[16], selected_codec)

    Note over Mac,Win: 2. Open Media TCP Connection
    Mac->>Win: 8B preface "WBRGMED\x01" + 16B session_id
    Win->>Mac: 1B status (0x00 = OK, 0x01 = Invalid Session)

    Note over Mac,Win: 3. Discover & Subscribe Windows
    Mac->>Win: ListWindowsRequest
    Win->>Mac: ListWindowsResponse
    Mac->>Win: SubscribeWindow(window_id, follow_popups=true)
    Win-->>Mac: [Media TCP] MediaFrameHeader(KEYFRAME) + payload
    Win-->>Mac: [Control TCP] WindowUpdated / CursorShape
```

- If the Control TCP connection closes, the host immediately closes the bound Media TCP connection and stops all active window capture sessions for that client.
- Keepalive: Client sends `Ping` every 5 seconds; if no traffic is received for 15 seconds, either side treats the session as dead.

---

## 2. Control Channel Framing

Immediately after the `WBRGCTL\x01` preface, both sides exchange length-prefixed Protobuf messages (defined in [control.proto](control.proto)):

```text
+---------------------------+----------------------------------------+
| uint32_be payload_len (4B)| Protobuf ControlMessage (payload_len B)|
+---------------------------+----------------------------------------+
```

- Maximum allowed `payload_len`: **16 MiB** (primarily to accommodate PNG clipboard images; anything larger MUST close the connection).

---

## 3. Media Channel Binary Frame Format

After the `WBRGMED\x01` + 16-byte `session_id` handshake returns `0x00`, Windows continuously writes frames with a **fixed 32-byte Big-Endian header** followed immediately by `payload_len` bytes of raw codec data:

| Offset | Size | Type | Field | Description |
| :--- | :--- | :--- | :--- | :--- |
| `0` | 4 | `u32` | `magic` | Fixed `0x57424D46` (`"WBMF"`) |
| `4` | 8 | `u64` | `window_id` | Windows `HWND` cast to `u64` |
| `12` | 8 | `u64` | `timestamp_us` | Host monotonic capture timestamp (microseconds) |
| `20` | 2 | `u16` | `width` | Frame width in physical pixels (for audio codecs `10`/`11`: sample rate in Hz, e.g. `48000`) |
| `22` | 2 | `u16` | `height` | Frame height in physical pixels (for audio codecs `10`/`11`: channel count, e.g. `2`) |
| `24` | 1 | `u8` | `codec` | `1`=H264, `2`=HEVC, `3`=JPEG, `4`=RAW_BGRA, `10`=AUDIO_PCM_S16LE, `11`=AUDIO_OPUS |
| `25` | 1 | `u8` | `flags` | Bitmask: `0x01`=KEYFRAME, `0x02`=CONFIG_ONLY, `0x04`=END_OF_STREAM, `0x08`=ALPHA_PNG |
| `26` | 2 | `u16` | `frame_seq` | Per-window wrapping frame counter (`0..65535`) |
| `28` | 4 | `u32` | `payload_len` | Byte length of the following payload (max **64 MiB**) |

### Codec Payload Rules
- **`H264_ANNEX_B (1)`**:
  - Encoded with **no B-frames**, low-latency profile (Baseline / Main / High).
  - Every frame with `KEYFRAME (0x01)` **MUST** inline `SPS` + `PPS` NAL units (with 4-byte Annex-B start codes `00 00 00 01`) before the `IDR` slice. This lets macOS `VideoToolbox` initialize or reconfigure its decoder cleanly on window open, resize, or `RequestKeyFrame`.
- **`HEVC_ANNEX_B (2)`**:
  - Same rule: inline `VPS` + `SPS` + `PPS` before every IDR frame.
- **`JPEG (3)` / `RAW_BGRA (4)`**:
  - Intended for **Day-1 MVP bring-up** before hardware encoding is wired up. Every frame sets `KEYFRAME (0x01)`. `RAW_BGRA` is tightly packed `width * height * 4` bytes (no row stride padding).

### Backpressure Policy (LAN)
- If a window's outbound media queue backs up (e.g. client decoding slower than 60 fps), **Windows Host drops intermediate non-keyframe frames** (or skips capturing new frames) rather than buffering infinitely and accumulating latency.

---

## 4. Coordinate System, Borders & DPI Contract

1. **Visible Window Bounds (Avoid the 7px Invisible Border Trap)**
   - On Windows 10/11, `GetWindowRect` includes invisible ~7px drop-shadow resize borders on left, right, and bottom.
   - Windows Host **MUST** compute `(x, y, width, height)` using `DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS)` and crop WGC frames (if needed) to match these exact visible bounds.
   - `SubscribeWindow.capture_mode` selects between `CAPTURE_MODE_WINDOW` (`CreateForWindow`, supports overlapped windows) and `CAPTURE_MODE_MONITOR_CROP` (`CreateForMonitor` cropped to the target window's visible rect — required for cross-process Android emulators like MuMu, Vulkan surfaces, and hardware overlays).
   - **Full Desktop Fallback**: Subscribing to the reserved synthetic `window_id = 0xFFFF_FFFF_FFFF_0001` streams the entire primary Windows monitor (taskbar, system tray, and all windows) with full mouse/keyboard/IME input.
2. **Coordinate Mapping for Mouse Events**
   - macOS sends normalized window-local coordinates `(norm_x, norm_y)` where `(0.0, 0.0)` is the top-left of the rendered window frame and `(1.0, 1.0)` is the bottom-right.
   - Windows converts this to virtual desktop coordinates using the target window's current visible rect:
     ```text
     abs_x = rect.left + round(norm_x * rect.width)
     abs_y = rect.top  + round(norm_y * rect.height)
     ```
   - Values slightly outside `[0.0, 1.0]` are permitted during active click-and-drag gestures (e.g. text selection or resizing internal split panes).
3. **Retina / DPI Scaling**
   - `WindowInfo.width` / `height` and `MediaFrameHeader.width` / `height` are always in **Windows physical pixels**.
   - macOS computes its local `NSWindow` content size in points as:
     ```text
     mac_points_w = window.width  / (window.dpi / 96.0)   // or / display_scale if Windows runs at 200% DPI
     ```
   - Best visual sharpness on a Retina Mac: set Windows display scaling to **200% (`dpi = 192`)** so 1 Windows physical pixel maps 1:1 to 1 macOS Retina pixel!

---

## 5. Window Lifecycle, Popups & Anti-Loop Resize

1. **Popup / Menu Tracking (`follow_popups = true`)**
   - Windows Host installs `SetWinEventHook` (`EVENT_OBJECT_CREATE`, `SHOW`, `HIDE`, `DESTROY`, `LOCATIONCHANGE`, `NAMECHANGE`).
   - When a tracked process spawns an owned dialog (`WINDOW_KIND_DIALOG`) or transient popup (`WINDOW_KIND_POPUP`: e.g. menu class `#32768`, dropdowns, tooltips), the host:
     1. Sends `WindowUpdated` with `owner_window_id`, `offset_x`, and `offset_y`.
     2. Immediately starts streaming frames for that popup's `window_id` on the Media channel.
   - macOS renders `WINDOW_KIND_POPUP` as a borderless, non-activating child window positioned at `(owner_mac_origin + scaled_offset)` above the parent window.
2. **Preventing Resize Feedback Loops**
   - macOS debounces live window drag resizing (e.g. every 30–50ms or on resize end) and sends `ResizeWindow(window_id, w, h, resize_seq)`.
   - Windows calls `SetWindowPos` and echoes `ack_resize_seq` in `WindowUpdated`.
   - While macOS is actively dragging a window edge, it ignores incoming `WindowUpdated` geometry changes whose `ack_resize_seq < local_latest_resize_seq`.
3. **Preventing WGC Minimize Freeze**
   - Minimizing an `HWND` on Windows causes WGC to stop producing frames.
   - When the user minimizes a remote window on macOS, macOS only hides/minimizes its own `NSWindow` and sends `UnsubscribeWindow` (or pauses streaming) without minimizing the actual Windows `HWND`.

---

## 6. Input & Chinese IME Rules

1. **Keyboard (`KeyEvent`)**
   - macOS translates `NSEvent.keyCode` → **USB HID Usage Page `0x07`** code.
   - Windows translates USB HID Usage → PS/2 Set-1 ScanCode (with `KEYEVENTF_EXTENDEDKEY` where needed) and injects via `SendInput(KEYEVENTF_SCANCODE)`.
   - Optional macOS-side convenience mapping (configurable in UI): `Cmd + C/V/X/Z/A/F/S` → `Ctrl + C/V/X/Z/A/F/S`; Cmd+W closes the local viewer.
2. **Chinese / CJK IME (`TextInput`)**
   - Do **not** rely on switching Windows IME remotely.
   - The macOS window implements `NSTextInputClient`; when the user commits composed text (e.g. Chinese characters), macOS sends `TextInput { window_id, text }`.
   - Windows encodes `text` to UTF-16 and injects each code unit with `SendInput` + `KEYEVENTF_UNICODE` (paired keydown + keyup).
3. **Focus Before Input**
   - On `MOUSE_EVENT_KIND_BUTTON_DOWN`, `KeyEvent`, or `TextInput`, if the target `window_id` (or its owner for popups) is not the Windows foreground window, Windows brings it to the foreground first before calling `SendInput`.
4. **High-Precision Trackpad Scroll**
   - macOS converts `NSEvent.scrollingDeltaY` / `scrollingDeltaX` into Windows wheel units (`120.0 = 1 notch`). Trackpad scaling is 12x; default compatibility mode accumulates complete 120-unit detents per view/axis. Users may select smooth small deltas instead. Windows retains fractional-unit remainder and injects integer values via `MOUSEEVENTF_WHEEL` / `MOUSEEVENTF_HWHEEL`; target applications determine support for partial detents. See [trackpad behavior](../docs/trackpad-scrolling.md).

---

## 7. v0.2 Additions & Future Evolution
- **Implemented in v0.2**:
  - **TLS 1.3 + TOFU pinning**, per-IP auth rate limiting, optional `--allow-cidr`, Named Pipe + `GetNamedPipeClientProcessId` secure-helper IPC, and macOS Keychain storage.
  - **mDNS / Bonjour** LAN auto-discovery (`_winbridge._tcp.local.`), exponential-backoff auto-reconnect with window restoration.
  - **HEVC (`VIDEO_CODEC_HEVC_ANNEX_B = 2`)** hardware encode/decode and **Opus (`AUDIO_CODEC_OPUS = 11`)** 48 kHz stereo 20 ms low-bandwidth audio alongside PCM lossless (`10`). `SetAudioConfig.scope = 1` mixes only subscribed window processes via WASAPI process loopback (Windows 10 20348+ / Windows 11). If that API is missing, a subscribed root is the desktop, or activation fails, the host falls back to system loopback and sends `HostError`. Scope `0` remains whole-system loopback. The host mixes every process into the existing single audio stream (`window_id = 0`) so the client ring buffer is unchanged.
  - **Smart 3D Game Relative Mouse Lock** (`MOUSE_EVENT_KIND_RELATIVE_MOVE = 5`), **Gamepad passthrough** (`GamepadState`), and **bidirectional chunked File Transfer** (`FileTransferStart` / `FileTransferChunk` / `FileTransferResult`; `direction = 2` is Windows → Mac, with `request_id` correlation for macOS `NSFilePromiseProvider` drag-out). `ListHostFiles` (`root:drop|desktop|downloads|documents` aliases) / `FilePullRequest` let the client browse Downloads, Desktop, and Documents (plus `Downloads\WinBridge`), pull files or on-the-fly `.zip` folders, through explicit selection. This fork rejects `path = "clipboard:"`, does not emit or consume `ClipboardFilesNotice`, and never publishes incoming uploads as `CF_HDROP`. All requested paths are canonicalized and rejected outside allowed roots.
  - **Per-window video profiles** (`SetVideoConfig`, field 45): text (≤30 fps), game (host fps cap), or auto, plus optional adaptive bitrate driven by the host media-queue backlog and the client's control-channel RTT (with dynamic `ICodecAPI` bitrate adjustment and cooldown hysteresis). A message with `profile = 0` and zero fps/bitrate only refreshes RTT.
- **Future (v0.3+)**:
  - Optional **QUIC** transport (`quinn` + `NWProtocolQUIC`) for per-window stream multiplexing without head-of-line blocking on lossy Wi-Fi / WAN links.


## Followed application windows and alpha (fork extension)

With `follow_popups=true`, the host follows explicit owner chains, including invisible
intermediate owners. It also follows a newly visible foreground normal window in the
subscribed application when there is exactly one eligible root. Windows visible when
the root was subscribed are excluded. Helper processes require a live, creation-time
validated parent chain in the same installation directory; names alone never match.
Automatically followed independent windows remain `NORMAL`; capture-group membership
must not make them owned panels. A nonzero association ID on a NORMAL window is not
presentation ownership. Owned/tool windows are `DIALOG` focusable panels; known
menus/tooltips and no-activate windows remain `POPUP`. `WS_EX_APPWINDOW` keeps an
ordinary owned window independent unless its owner is disabled (modal). Disabling tracking, ambiguous roots, and
unrelated applications never opt into this new association path.

`Hello.capabilities` and `HelloAck.capabilities` bit 8 advertise `ALPHA_PNG`. Only a
client advertising this bit receives media flag `0x08`. The codec byte is unchanged.
The payload then contains `u32 big-endian color_length`, `color_length` bytes of the
usual codec payload, and one lossless **8-bit grayscale PNG** containing alpha. PNG
width/height must equal the media header. Alpha and color are one queue item; every
flagged frame is self-contained, including repeated masks after dropped frames.
The host matches asynchronous encoder output by MFT sample timestamp; it does not
shift the next mask forward when an encoder skips an input. Missing timestamps are
rejected for alpha-enabled capture instead of attaching an unverified mask.
The 64 MiB total payload limit still applies; alpha PNG is limited to 8 MiB and the
alpha surface to 16,777,216 pixels. No flag means opaque/ordinary codec behavior;
clients must not retain the previous frame's mask. EOS has no alpha envelope.

Window-mode WGC capture preserves alpha for followed windows, dialogs/popups and
layered roots. Opaque pixels do not produce a sidecar. Screen-crop capture includes
the desktop composition and does not carry window alpha. WGC premultiplied color
components are preserved through the codec; the client replaces alpha, without a
second RGB multiplication or treating black as transparent. Color compression is
still lossy under JPEG/H.264/HEVC. Opaque normal windows retain the GPU-only host
path and normal IOSurface presentation. Eligible windows require GPU readback for
the alpha plane; masked frames currently use CPU composition on macOS. This is not
a zero-copy alpha pipeline or a guaranteed high-FPS result.

中文：开启“追踪弹窗”后，新增的前台普通窗口也可跟随同一应用的唯一投射根窗口，
使用可聚焦的 Mac 标题栏窗口；菜单仍按浮层展示。不会顺带打开投射前已存在的窗口。
同目录辅助进程还必须通过真实父子关系和创建时间验证，不能只看进程名。
透明度通过能力位 8 协商，以媒体标志 `0x08` 将颜色码流与灰度 PNG 遮罩封装在同一帧，
丢帧不会使遮罩错配；不带标志的帧清除旧遮罩。保留黑色内容和半透明边缘，不使用黑色抠图。
透明窗口当前增加 GPU 回读和 Mac CPU 合成成本；普通不透明主窗口维持原路径。

### Local viewer lifecycle / 本地查看窗口生命周期

WindowInfo fields 20 `is_disabled` and 21 `is_modal` default to false for old peers.
The host obtains enabled state from Windows and rechecks it before input/window
actions, permitting key/button releases. A locally hidden modal still blocks its
Windows owner. It never implies a host CloseWindow or an UnsubscribeWindow.

Mac close hides the selected viewer and the transitive DIALOG/POPUP ownership subtree.
NORMAL windows are independent even within one capture group. Hidden ancestors also
suppress subsequently discovered owned windows. Updates and late video do not reopen
hidden viewers. Restoring a child first restores its ownership ancestors; manager
and menu actions restore visibility and request fresh keyframes. Actual host removal
cleans the local subtree, while loss of a capture root promotes a surviving independent
member. Explicit unsubscribe still ends the capture group. Disconnect clears local
visibility preferences; hidden streams are still decoded in this implementation.

Mac 关闭只隐藏本地窗口及所属子树，独立成员保留。迟到视频和目录更新不会重新弹出；
隐藏拥有者的新浮窗也保持隐藏。恢复子窗会先恢复拥有者。关闭本地模态对话框不解除
Windows 的父窗口阻断；输入和主机尺寸操作由服务端复查，按键／鼠标释放仍允许。
标题栏最小化、最大化保持本地操作，开启同步缩放后也只有手动拖动边框才请求主机尺寸。


## Scoped clipboard (fork extension)

Capability bit 9 (`SCOPED_CLIPBOARD`) requires both updated endpoints. WindowInfo
field 22 `clipboard_token` is a nonzero subscription generation, replaced after
unsubscribe/reopen. It is not an authentication secret. The authenticated control
channel carries ClipboardRequest (field 80) and ClipboardResult (field 81).
Requests include monotonically increasing request_id, window_id, token and action
(`copy`, `cut`, `paste`, `poll`). Only paste has ClipboardSync data; poll may set
`discard=true` to establish a baseline without returning existing contents.
Results echo identity/action and carry success, error and optional text/PNG data.

The host rejects unsubscribed/stopped/reused windows, stale tokens, desktop targets,
replayed requests and legacy unscoped ClipboardSync. It validates exact foreground,
enabled state and process creation time. Reads additionally require a clipboard
owner belonging to the subscribed capture group; no clipboard-file reads occur.
Paste publishes validated data before injecting the shortcut. Copy/cut wait for a
new clipboard sequence; polling never returns an initial clipboard value.

The Mac accepts only its matching live request while the same viewer remains key,
visible and enabled. It preserves any newer local clipboard change. Disconnect,
local close, focus loss and disabling sync invalidate pending work. It never
exports local changes in the background or accepts unsolicited legacy updates.
Text is at most 1 MiB UTF-8; PNG at most 8 MiB with at most 16 Mi pixels decoded.
Explicit file transfers retain separate, guarded publication tickets.

See [design and validation](../docs/scoped-clipboard.md) for privacy boundaries.
