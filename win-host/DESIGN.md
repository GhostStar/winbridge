# WinBridge Windows 被控端（`win-host`）开发设计文档

> **文档用途**：本文档面向在 Windows 环境下开发的工程师 / AI 编程助手，用于从零实现 `win-host`（Rust）。
> **强约束**：本项目为跨端系统（Windows Rust 被控端 + macOS Swift 主控端），**必须严格遵守**仓库中已定稿的协议文件：
> - `../protocol/control.proto`
> - `../protocol/SPEC.md`
>
> 未经用户确认，**不得私自更改** `control.proto` 中的字段编号、消息类型、8 字节连接前缀或 32 字节媒体帧二进制头格式。

---

## 1. 产品背景与运行环境

- **核心目标**：在局域网（LAN）内，将 Windows 上用户指定的**单个或多个应用窗口**（及其右键菜单、下拉框、对话框等弹出窗口）低延迟投射到 macOS 上，像原生 macOS 窗口一样浏览和操作。
- **典型使用场景**：Windows 主机插有物理显示器，但平时显示器输入源会切换到 Mac，Windows 处于**无人值守的当前交互式桌面会话**中（单用户，不需要多会话并发）。
- **目标操作系统**：Windows 10 1903+ / Windows 11（64 位 `x86_64-pc-windows-msvc`）。
- **目标语言与工具链**：Rust（Edition 2021，MSVC toolchain）。

---

## 2. 技术栈与推荐依赖（`Cargo.toml`）

```toml
[package]
name = "win-host"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
prost = "0.13"
bytes = "1"
clap = { version = "4", features = ["derive"] }
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
uuid = { version = "1", features = ["v4"] }
parking_lot = "0.12"
crossbeam-channel = "0.5"
# MVP 阶段快速编码用（Milestone 2）；后续 Milestone 3 接入硬件 H.264 编码后作为降级/调试编码器保留
jpeg-encoder = "0.6"
# 仅用于导出窗口图标 PNG、剪贴板 PNG 及 Milestone 1 测试存图
png = "0.17"

[dependencies.windows]
version = "0.58"
features = [
    "Foundation",
    "Graphics",
    "Graphics_Capture",
    "Graphics_DirectX",
    "Graphics_DirectX_Direct3D11",
    "Win32_Foundation",
    "Win32_Graphics_Direct3D",
    "Win32_Graphics_Direct3D11",
    "Win32_Graphics_Dxgi",
    "Win32_Graphics_Dxgi_Common",
    "Win32_Graphics_Dwm",
    "Win32_Graphics_Gdi",
    "Win32_Media_MediaFoundation",
    "Win32_Security",
    "Win32_System_Com",
    "Win32_System_DataExchange",
    "Win32_System_Memory",
    "Win32_System_ProcessStatus",
    "Win32_System_Threading",
    "Win32_System_WinRT",
    "Win32_System_WinRT_Direct3D11",
    "Win32_System_WinRT_Graphics_Capture",
    "Win32_UI_Accessibility",
    "Win32_UI_HiDpi",
    "Win32_UI_Input_KeyboardAndMouse",
    "Win32_UI_WindowsAndMessaging",
]

[build-dependencies]
prost-build = "0.13"
# 内置 protoc，避免在 Windows 上手动配置 protoc.exe 环境变量
protoc-bin-vendored = "3"
```

### `build.rs` 参考实现
```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    std::env::set_var("PROTOC", protoc);
    println!("cargo:rerun-if-changed=../protocol/control.proto");
    prost_build::compile_protos(&["../protocol/control.proto"], &["../protocol"])?;
    Ok(())
}
```

---

## 3. 总体架构与线程模型（关键！）

Win32 / COM / WinRT 对线程公寓模型（STA / MTA）和消息循环有严格要求，**切忌把所有 Win32 调用直接塞进 Tokio 工作线程**。必须拆分为以下 4 个清晰隔离的执行域：

```text
                       ┌─────────────────────────────────────────────┐
                       │         Tokio Async Runtime (TCP I/O)       │
  macOS Client ◄──────►│  • Single TcpListener (--host / --port 9876)│
  (Control + Media)    │  • 8B Preface Router (WBRGCTL / WBRGMED)    │
                       │  • Session Manager & Per-Window Fair Muxer  │
                       └──────┬───────────────▲──────────────▲───────┘
                              │ Commands      │ Encoded      │ Window/Cursor/
                              ▼               │ Frames       │ Clipboard Events
          ┌───────────────────────┐  ┌────────┴─────────┐  ┌─┴────────────────────┐
          │ Input Injector Thread │  │ Capture & Encode │  │ Win32 UI Hook Thread │
          │ (Dedicated OS Thread) │  │ Workers (MTA)    │  │ (Dedicated STA +     │
          │ • SetForegroundWindow │  │ • D3D11 Device   │  │  GetMessageW Loop)   │
          │ • SendInput (Mouse,   │  │ • WGC FreeThread │  │ • SetWinEventHook    │
          │   HID->ScanCode, IME) │  │ • MF H264 / JPEG │  │ • Cursor Tracker     │
          │ • SetWindowPos        │  │ • IDR / Resize   │  │ • Clipboard Listener │
          └───────────────────────┘  └──────────────────┘  └──────────────────────┘
```

### 进程启动第 0 步（必须在 `main()` 最顶部执行）
在创建任何窗口、调用任何 Win32 / WGC API 之前，必须开启 Per-Monitor V2 DPI 感知：
```rust
unsafe {
    let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
}
```
否则在高分屏（125%/150%/200%）下，`DwmGetWindowAttribute`、WGC 帧尺寸和 `SendInput` 坐标会全部出现缩放错位。

---

## 4. 建议代码目录结构

```text
win-host/
├── Cargo.toml
├── build.rs
├── DESIGN.md                 # 本文档
└── src/
    ├── main.rs               # CLI 参数解析、DPI 初始化、启动各子系统
    ├── config.rs             # CLI/配置结构体（--bind, --port, --token, --fps, --bitrate, --codec 等）
    ├── proto.rs              # include!(concat!(env!("OUT_DIR"), "/winbridge.v1.rs")) + 32B 媒体头编解码
    ├── session.rs            # 会话状态机（session_id 绑定、多窗口订阅表、弹窗归属映射）
    ├── net/
    │   ├── mod.rs
    │   ├── server.rs         # 单端口 TcpListener（TCP_NODELAY）+ 8 字节前缀路由
    │   ├── control.rs        # Control TCP 长度前缀读写（上限 16 MiB）+ 消息分发
    │   └── media.rs          # Media TCP 握手（session_id 校验 + 0x00/0x01）+ 按窗口公平调度发送器
    ├── win32/
    │   ├── mod.rs
    │   ├── window.rs         # EnumWindows、DWMWA_EXTENDED_FRAME_BOUNDS、窗口过滤、图标提取、Resize/Focus/Close
    │   ├── event_hook.rs     # STA 线程消息循环 + SetWinEventHook（追踪主窗变化与弹出窗口 POPUP/DIALOG）
    │   ├── cursor.rs         # GetCursorInfo + 系统光标句柄映射为 CursorType
    │   └── clipboard.rs      # HWND_MESSAGE 隐藏窗口 + AddClipboardFormatListener 双向文本/PNG 同步
    ├── capture/
    │   ├── mod.rs
    │   ├── d3d11.rs          # D3D11 Device（BGRA_SUPPORT + MultithreadProtected）与 WinRT IDirect3DDevice 互操作
    │   └── wgc.rs            # IGraphicsCaptureItemInterop + CreateFreeThreaded 捕获池 + 可见边框裁剪 + Resize 重建
    ├── encode/
    │   ├── mod.rs            # FrameEncoder trait 定义（encode / force_keyframe / reconfigure）
    │   ├── raw_bgra.rs       # D3D11 Staging Texture 读回 -> RAW_BGRA（调试）
    │   ├── jpeg.rs           # D3D11 Staging Texture -> jpeg-encoder（Milestone 2 MVP）
    │   └── mf_h264.rs        # Windows Media Foundation H.264 硬件编码器（GPU NV12 转换 + 零 B 帧低延迟 + 内联 SPS/PPS）
    └── input/
        ├── mod.rs
        ├── injector.rs       # 归一化坐标换算虚拟桌面坐标 + SetForegroundWindow + SendInput（鼠/键/Unicode/滚轮）
        └── hid_scancode.rs   # 完整 USB HID Usage Page 0x07 -> PS/2 Set-1 ScanCode（含 0xE0 扩展键）映射表
```

> **代码隔离原则**：所有 `unsafe` 的 Win32 / COM / WinRT 调用必须封装在 `win32/`、`capture/`、`encode/`、`input/` 内部，对外仅暴露安全的 Rust 结构体和 Result；`net/` 和 `session.rs` 中不应出现 `unsafe`。

---

## 5. 协议核心契约速查（与 `protocol/SPEC.md` 严格一致）

### 5.1 单端口双 TCP 路由
- 监听地址默认 `0.0.0.0:9876`（通过 `--bind` 和 `--port` 可配置）。
- 每个 accepted TCP socket 必须立即设置 `socket.set_nodelay(true)`。
- 读取前 **8 字节**：
  - `b"WBRGCTL\x01"` → 进入 Control 信道：循环读写 `[4B Big-Endian u32 len][Protobuf ControlMessage]`（`len > 16 MiB` 直接断连）。
  - `b"WBRGMED\x01"` → 再读 **16 字节 `session_id`**：
    - 若匹配当前活跃会话且尚未绑定媒体连接：回写 1 字节 `0x00`，进入媒体推流循环。
    - 否则：回写 1 字节 `0x01` 并关闭连接。
  - 其他前缀 → 直接关闭连接。
- Control TCP 断开时，必须立即关闭对应的 Media TCP 并停止该会话的所有窗口采集与编码任务。

### 5.2 Media 信道 32 字节固定二进制头（Big-Endian）
```rust
pub const MEDIA_FRAME_MAGIC: u32 = 0x5742_4D46; // "WBMF"
pub const FLAG_KEYFRAME: u8 = 0x01;
pub const FLAG_CONFIG_ONLY: u8 = 0x02;
pub const FLAG_END_OF_STREAM: u8 = 0x04;

pub struct MediaFrameHeader {
    pub magic: u32,         // offset 0..4   固定 0x57424D46
    pub window_id: u64,     // offset 4..12  HWND as u64
    pub timestamp_us: u64,  // offset 12..20 Instant 单调微秒时间戳
    pub width: u16,         // offset 20..22 帧物理宽
    pub height: u16,        // offset 22..24 帧物理高
    pub codec: u8,          // offset 24     1=H264, 2=HEVC, 3=JPEG, 4=RAW_BGRA
    pub flags: u8,          // offset 25     FLAG_* 位掩码
    pub frame_seq: u16,     // offset 26..28 单窗口自增帧序号（wrapping_add）
    pub payload_len: u32,   // offset 28..32 紧随其后的负载字节数（<= 64 MiB）
}
```

### 5.3 多窗口发送调度与背压（防止大窗口饿死小窗口/弹窗）
在 `net/media.rs` 中，**不要**让所有窗口往同一个无界 `mpsc` 里盲塞帧：
- 为每个活跃 `window_id` 维护一个**极小槽位（最新帧缓冲）**：
  - 对于 `JPEG` / `RAW_BGRA`：槽位容量为 1，新帧直接覆盖未发出的旧帧。
  - 对于 `H264_ANNEX_B` / `HEVC_ANNEX_B`：因为 P 帧依赖前一帧，**不能**随意丢弃中间 P 帧；若单窗口待发队列积压超过阈值（例如 3 帧），通知采集端暂缓送编码，或丢弃积压队列并立即触发编码器输出一个新的 IDR 关键帧。
- Media 写循环对所有有待发帧的 `window_id` 做**轮询（Round-Robin）**发送，并且给予 `WINDOW_KIND_POPUP` 更高优先级，保证右键菜单/下拉框永远零延迟弹出。

---

## 6. Windows 底层关键实现规范与避坑清单

### 6.1 窗口枚举与可见边框计算（`win32/window.rs`）
1. **过滤规则（列出有意义的顶层应用窗口）**：
   - `IsWindowVisible(hwnd)` 为真。
   - 排除 `DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED)` 非零的窗口（过滤掉 Win10/11 后台挂起的 UWP 幽灵窗口，如不可见的 Settings/Calculator）。
   - 排除 `WS_EX_TOOLWINDOW`（除非被作为弹出窗口追踪）以及宽高为 `0` 的窗口。
   - 排除标题为空且非主窗口的辅助窗口，排除 Shell 桌面窗口（`Progman`、`WorkerW`）。
2. **真实可见矩形（极其重要！）**：
   - 必须使用 `DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS)` 获取 `(left, top, right, bottom)`，失败时才回退 `GetWindowRect`。
   - **Resize 时的尺寸换算**：当收到 macOS 的 `ResizeWindow(hwnd, target_visible_w, target_visible_h)` 时，由于 `SetWindowPos` 接受的是包含隐形边框的窗口外框尺寸，需先计算隐形边框差值：
     ```text
     pad_w = (win_rect.right - win_rect.left) - (ext_rect.right - ext_rect.left)
     pad_h = (win_rect.bottom - win_rect.top) - (ext_rect.bottom - ext_rect.top)
     SetWindowPos(hwnd, ..., target_visible_w + pad_w, target_visible_h + pad_h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE)
     ```
   - 调用 `SetWindowPos` 后立刻回发 `WindowUpdated`，其中 `ack_resize_seq = req.resize_seq`。

### 6.2 WGC 单窗口采集（`capture/wgc.rs`）
1. **COM/WinRT 初始化**：采集线程必须以 `RoInitialize(RO_INIT_MULTITHREADED)` 初始化。
2. **无消息循环坑（必看！）**：
   - 必须使用 **`Direct3D11CaptureFramePool::CreateFreeThreaded`**，**严禁**使用 `Create`（`Create` 要求线程绑定 `DispatcherQueue`，在无 WinUI 的 CLI 程序里会直接卡死不出帧！）。
3. **从 `HWND` 创建 `GraphicsCaptureItem`**：
   - 通过 `windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()` 调用 `CreateForWindow(hwnd)`。
4. **采集会话配置**：
   - `session.SetIsBorderRequired(false)`：尝试关闭 Win11 黄色录制边框（在低版本 Win10 上若返回错误则忽略继续）。
   - `session.SetIsCursorCaptureEnabled(false)`：默认不在视频帧里画光标，避免与 macOS 原生光标重影（可留 CLI 开关 `--capture-cursor` 便于调试）。
5. **隐形边框裁剪**：
   - WGC 抓出的 `ID3D11Texture2D` 尺寸有时对应 `GetWindowRect`（包含左侧/底部 7px 透明阴影边框）。
   - 计算偏移：`crop_x = (ext_rect.left - win_rect.left).max(0)`、`crop_y = (ext_rect.top - win_rect.top).max(0)`、`crop_w = ext_rect.width().min(tex_w - crop_x)`、`crop_h = ext_rect.height().min(tex_h - crop_y)`。
   - 使用 `ID3D11DeviceContext::CopySubresourceRegion` 裁剪到目标纹理（宽/高需注意 H.264 编码器通常要求偶数宽高，可在编码前向下或向上对齐到 2 的倍数并在 `MediaFrameHeader` 中填实际编码宽高）。
6. **窗口最小化 / 显示器切源处理**：
   - 若 `IsIconic(hwnd)` 为真，发送 `HostError { window_id, code: CAPTURE_PAUSED_MINIMIZED }`（避免死等帧）。
   - 若 `EnumDisplayMonitors` 数为 0 或 D3D11/WGC 报设备丢失，发送 `HostError { code: DISPLAY_DISCONNECTED }` 并打印明确的日志提示（排查 DP 切源断连 / 虚拟显示器驱动）。
7. **动态 Resize 处理**：
   - 每帧检查 `frame.ContentSize()` 是否与当前帧池尺寸不同；若变化，调用 `frame_pool.Recreate(...)` 重建帧池并触发编码器 `reconfigure(new_w, new_h)` + 强制出关键帧。

### 6.3 硬件 H.264 低延迟编码器（`encode/mf_h264.rs`，Milestone 3）
- 启动时初始化 `MFStartup(MF_VERSION, MFSTARTUP_FULL)`。
- **GPU 零拷贝色彩转换**：WGC 产出的是 `DXGI_FORMAT_B8G8R8A8_UNORM`，Media Foundation H.264 编码器输入通常需要 `NV12`。使用 `ID3D11VideoDevice` + `ID3D11VideoContext::VideoProcessorBlt` 在 GPU 内直接完成 `BGRA -> NV12` 转换，避免 CPU 色彩转换开销。
- **选择编码器**：用 `MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, ...)` 优先枚举 NVENC / Intel QSV / AMD AMF 硬件 MFT；若硬件不可用，优雅回退至软件 H.264 MFT（`MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT`）。
- **必须开启的低延迟属性（`ICodecAPI`）**：
  - `CODECAPI_AVLowLatencyMode = VARIANT_TRUE`
  - `CODECAPI_AVEncMPVDefaultBPictureCount = 0`（**禁用 B 帧**，保证零重排延迟）
  - `CODECAPI_AVEncCommonMeanBitRate = bitrate_bps`（内网建议默认 10~20 Mbps）
  - 收到 `RequestKeyFrame`、首帧或 Resize 重建时设置 `CODECAPI_AVEncVideoForceKeyFrame = 1`。
- **SPS / PPS 内联要求（对 macOS `VideoToolbox` 至关重要）**：
  - 解析编码器输出的 H.264 Annex-B 字节流中的 NALU 类型（`nal_header & 0x1F`）：缓存最新的 `SPS (7)` 和 `PPS (8)`。
  - 每当输出帧包含 `IDR (5)` 时，标记 `FLAG_KEYFRAME (0x01)`，并**确保该帧负载以 `[00 00 00 01][SPS][00 00 00 01][PPS][00 00 00 01][IDR...]` 开头**（如果编码器未自带 SPS/PPS，由编码包装层自动前置拼入）。

### 6.4 键鼠与中文输入法注入（`input/injector.rs` & `input/hid_scancode.rs`）
1. **焦点与权限检测**：
   - 在注入鼠标点击（`BUTTON_DOWN`）、按键或文本前，若目标窗口（或 `POPUP` 的 `owner_window_id`）不是当前前景窗口，则先调用 `SetForegroundWindow(target)`。
   - 如果目标进程属于管理员提权（UIPI 拦截导致普通权限 `win-host` 无法注入），检测后发送 `HostError { window_id, code: ACCESS_DENIED_ELEVATED }` 提示用户（可提示以管理员身份运行 `win-host`）。
2. **鼠标坐标映射到多显示器虚拟桌面**：
   - 获取虚拟桌面范围：`vleft = GetSystemMetrics(SM_XVIRTUALSCREEN)`、`vtop = GetSystemMetrics(SM_YVIRTUALSCREEN)`、`vwidth = GetSystemMetrics(SM_CXVIRTUALSCREEN)`、`vheight = GetSystemMetrics(SM_CYVIRTUALSCREEN)`。
   - 根据窗口最新 `DWMWA_EXTENDED_FRAME_BOUNDS` 算出像素坐标 `(px, py)`，再转换为 `SendInput` 的 `[0, 65535]` 归一化坐标：
     ```text
     dx = ((px - vleft) * 65535) / (vwidth - 1).max(1)
     dy = ((py - vtop)  * 65535) / (vheight - 1).max(1)
     ```
   - 标志位必须带 `MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`（不带 `VIRTUALDESK` 在多屏或负坐标副屏下会严重偏位！）。
3. **高精度滚轮**：
   - `wheel_delta_y.round() as i32` 直接写入 `MOUSEINPUT.mouseData` 配合 `MOUSEEVENTF_WHEEL`（水平滚动用 `MOUSEEVENTF_HWHEEL`），保留 macOS 触控板细腻的平滑滚动手感。
4. **键盘（USB HID Usage Page 0x07 -> PS/2 Set-1 ScanCode）**：
   - 实现完整静态查表 `hid_usage_to_scancode(hid: u32) -> Option<(u16, bool /* is_extended */)>`：
     - 覆盖字母 `A-Z (0x04..=0x1D)`、数字 `1-0 (0x1E..=0x27)`、功能键 `F1-F12 (0x3A..=0x45)`、控制键 `Enter/Esc/Backspace/Tab/Space`、标点、方向键、Home/End/PgUp/PgDn/Ins/Del、左右 Shift/Ctrl/Alt/Win、小键盘。
     - 对扩展键（方向键、Home/End/Ins/Del/PgUp/PgDn、Right Ctrl、Right Alt、LWin/RWin、Numpad Enter/Divide）必须置位 `KEYEVENTF_EXTENDEDKEY`。
   - 调用 `SendInput` 时使用 `KEYEVENTF_SCANCODE`（抬起加 `KEYEVENTF_KEYUP`）。
5. **中文输入法（`TextInput`）**：
   - 将 UTF-8 `text` 转为 `text.encode_utf16()`，对每个 `u16` 码元生成一对 `KEYBDINPUT`（`wVk = 0`, `wScan = code_unit`, `dwFlags = KEYEVENTF_UNICODE` 以及 `KEYEVENTF_UNICODE | KEYEVENTF_KEYUP`），一次性批量传入 `SendInput`（支持含 Emoji 的 UTF-16 代理对）。

### 6.5 弹出窗口自动追踪（`win32/event_hook.rs`，Milestone 4）
- 专用 STA 线程运行 `GetMessageW` / `DispatchMessageW`，注册 `SetWinEventHook(EVENT_MIN, EVENT_MAX, ..., WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS)`（或精准订阅 `EVENT_OBJECT_CREATE`、`DESTROY`、`SHOW`、`HIDE`、`LOCATIONCHANGE`、`NAMECHANGE`、`EVENT_SYSTEM_FOREGROUND`）。
- 当某窗口被订阅且 `follow_popups == true` 时，记录其 `pid` 和 `hwnd`：
  - 同一 `pid` 下出现新的可见窗口时，检查 `GetWindow(child, GW_OWNER)` 或窗口类名/样式（如 `#32768` 系统菜单、`WS_POPUP`、`WS_EX_TOOLWINDOW`、对话框 `#32770`、Chromium/Electron 下拉浮窗）：
    - 判定为 `WINDOW_KIND_POPUP`（无标题栏浮层菜单/下拉/Tooltip）或 `WINDOW_KIND_DIALOG`（模态/非模态对话框）。
    - 计算相对主窗口偏移 `offset_x = child.x - owner.x`、`offset_y = child.y - owner.y`。
    - 自动下发 `WindowUpdated` 并立刻为其启动 WGC 采集推流；当其 `HIDE` 或 `DESTROY` 时停止采集并下发 `WindowClosed` + `MediaFrame(FLAG_END_OF_STREAM)`。

---

## 7. 分阶段实施路线与验收标准（Milestones）

> **给 AI 编程助手的指令**：请严格按以下 4 个 Milestone 顺序推进。每完成一个 Milestone，必须确保 `cargo check` 和 `cargo test` 通过，并向用户给出可在 Windows 终端直接运行的验证命令。

### Milestone 1：Win32 & WGC 基础探针（验证硬件与显示器切源）
- **交付物**：
  - 初始化 `win-host` 工程，`build.rs` 成功编译 `../protocol/control.proto`。
  - 实现 CLI 子命令/模式：
    1. `win-host --list-windows`：打印当前所有可投射窗口（`HWND`、`PID`、进程名、标题、可见宽高、DPI）。
    2. `win-host --test-capture <HWND> --frames 30 --save test.png`：通过 WGC `CreateFreeThreaded` 抓取 30 帧，输出实测平均 FPS、单帧耗时、裁剪后分辨率，并保存最后一帧为 `test.png`。
    3. `win-host --test-input <HWND>`：对该窗口测试一次置前、输入一段中英文混合文字（验证 `KEYEVENTF_UNICODE`）和一次 `Resize`。
- **验收标准**：
  - `test.png` 打开清晰无黑边/无 7px 偏移（测试记事本 + Chrome/Edge 两个窗口）。
  - **关键实机验证**：把显示器输入源切到 Mac 后（可通过延迟 5 秒执行 `--test-capture`），确认 Windows 是否依然能正常出帧；若触发 DP 断连，日志能明确报出显示器状态。

### Milestone 2：完整网络服务 + 多窗口 JPEG/RAW 串流 + 全套操控（可与 macOS 联调）
- **交付物**：
  - `win-host --bind 0.0.0.0 --port 9876 --token <secret>` 正式服务模式。
  - 完整实现单端口双 TCP 协议（`WBRGCTL\x01` + `WBRGMED\x01` + `session_id` 绑定 + 32 字节帧头）。
  - 支持多窗口并发 `SubscribeWindow` / `UnsubscribeWindow`，优先用 `VIDEO_CODEC_JPEG`（及 `RAW_BGRA`）推流，实现按窗口公平调度与背压丢旧帧。
  - 完整实现 `ListWindows`、`ResizeWindow`（带 `ack_resize_seq`）、`FocusWindow`、`CloseWindow`、`WindowAction`、`MouseEvent`、`KeyEvent`（完整 HID->ScanCode 表）、`TextInput`、`Ping/Pong`。
  - 编写本地端到端自动化测试（`tests/`）：连上 `127.0.0.1`，完成握手、拉列表、订阅窗口并校验收到的 `WBMF` 32 字节帧头与负载合法性。
- **验收标准**：
  - 能同时订阅 2 个以上窗口，独立接收各自帧流且互不卡死。
  - macOS 端（或测试脚本）发来点击/滚轮/快捷键/中文输入，目标 Windows 窗口精准响应，延迟低、坐标无偏位。

### Milestone 3：Media Foundation H.264 硬件低延迟编码
- **交付物**：
  - 实现 `encode/mf_h264.rs`（GPU `BGRA -> NV12` + 硬件 H.264 MFT，无硬件时回退软件 MFT/JPEG）。
  - 强制 0 B 帧低延迟模式；每个 IDR 帧前置 `SPS + PPS` Annex-B NALU 并置位 `FLAG_KEYFRAME`。
  - 完整支持 `RequestKeyFrame` 和窗口 Resize 后的编码器动态重配（`reconfigure`）。
- **验收标准**：
  - 1080p 窗口 60fps 持续推流时，Windows CPU 占用率 < 10%，单帧抓图+编码耗时 < 8ms。
  - 将收到的单窗口裸 H.264 流落盘为 `.h264`，用 `ffplay` 可直接流畅播放无花屏。

### Milestone 4：无缝窗口体验补完（Popup 弹窗 + 光标同步 + 剪贴板）
- **交付物**：
  - `SetWinEventHook` 追踪被订阅窗口/进程的 `DIALOG` 与 `POPUP`（右键菜单、下拉列表、Tooltip），自动计算 `offset_x / offset_y` 并同步推流与销毁。
  - `CursorShape` 变化推送（箭头、工字光标、手型、缩放双箭头等）。
  - `ClipboardSync` 双向同步（UTF-8 文本 + PNG 图片，带防回环机制）。
- **验收标准**：
  - 在 Chrome / VS Code / 记事本中右键弹出菜单、点击顶部菜单栏或弹出“另存为”对话框时，能实时收到对应的 `WindowUpdated(POPUP/DIALOG)` 及其画面帧，菜单消失后收到 `WindowClosed`。
  - 鼠标移入输入框/链接/窗口边缘时能收到对应的 `CursorShape` 更新。

---

## 8. CLI 参数设计建议

```text
win-host [OPTIONS]

Options:
  --bind <ADDR>            监听地址 [default: 0.0.0.0]
  --port <PORT>            监听端口（Control + Media 共用）[default: 9876]
  --token <SECRET>         内网预共享鉴权口令（空字符串表示开发模式免鉴权）[default: ""]
  --fps <FPS>              默认单窗口最大帧率 [default: 60]
  --bitrate <KBPS>         默认 H.264 目标码率（kbps）[default: 15000]
  --codec <CODEC>          首选编码器: auto | h264 | jpeg | raw [default: auto]
  --capture-cursor         是否在视频画面中绘制 Windows 光标（调试用）[default: false]
  --log-level <LEVEL>      日志级别: error | warn | info | debug [default: info]

Diagnostics (Milestone 1):
  --list-windows           列出当前可见窗口并退出
  --test-capture <HWND>    测试抓取指定窗口（支持十进制或 0x 十六进制）
  --delay-secs <N>         测试抓帧前延迟秒数（用于测试显示器切给 Mac 后的抓帧表现）[default: 0]
  --frames <N>             测试抓取帧数 [default: 30]
  --save <PATH>            将测试抓到的最后一帧保存为 PNG
```

---

## 9. v0.2 架构演进与安全实现说明

- **Session 0 服务 + Session 1 双进程协同模型（`service.rs` / `helper.rs`）**：
  - `WinBridgeHost` Windows Service 以 `LocalSystem` 运行在 Session 0，仅在 `C:\ProgramData\WinBridge\service.conf`（SDDL 仅限 `SY`/`BA`）保存配置与口令，SCM `ImagePath` 仅保留 `"<exe>" --service`。
  - 针对当前活跃交互式桌面会话，服务先以 `CREATE_SUSPENDED` 启动当前登录用户身份的 `--session-worker`（通过继承的匿名 `stdin` 管道传递 Token 与 TLS 私钥），拿到精确 `worker_pid` 与用户 SID 后，再启动 `LocalSystem + TokenUIAccess=1` 的 `--secure-helper` 监听 SDDL 保护的 Windows Named Pipe（`PIPE_REJECT_REMOTE_CLIENTS`）并严格校验 `GetNamedPipeClientProcessId == worker_pid`，最后 `ResumeThread` 唤醒 Worker。
- **TLS 1.3 + 防爆破 + CIDR 白名单（`net/tls.rs` / `security.rs` / `net/mod.rs`）**：
  - 使用 `rcgen`（ECDSA P-256）自动生成持久化自签证书，`rustls`（`ring`）+ `tokio-rustls` 加密 Control 与 Media 通道，并提供常量时间口令校验、按 IP 指数退避锁定和 `--allow-cidr`。
- **LAN 自动发现、HEVC、Opus、文件拖放与手柄透传**：
  - `net/discovery.rs`：纯 Rust `mdns-sd` 广播 `_winbridge._tcp.local.`。
  - `encode/annexb.rs` & `encode/mf_h264.rs`：支持硬件 HEVC（`MFVideoFormat_HEVC` + `VPS/SPS/PPS` 内联）与 H.264 零 B 帧编码。
  - `audio/mod.rs`：WASAPI 环回 + 48 kHz 重采样 + 纯 Rust `mousiki` Opus 编码（`codec=11`）及无损 PCM（`codec=10`）。
  - `win32/files.rs`：安全接收 macOS 拖放文件至 `Downloads\WinBridge\`（文件名净化 + 原子重命名 + 512 MiB 上限）。
  - `input/gamepad.rs`：运行时动态加载 `ViGEmClient.dll` 提供可选虚拟 Xbox 360 手柄支持，未安装驱动时零构建/启动依赖。

