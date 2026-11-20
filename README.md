# WinBridge

面向局域网（LAN）的**无缝单窗口远程操控工具**：将 Windows 上的单个或多个应用窗口以低延迟投射到 macOS 上，像原生 macOS 窗口一样浏览、缩放、输入中文与键鼠操控。

> 典型场景：Windows 主机与 Mac 共用显示器，平时显示器切给 Mac 使用，无需来回切换显示器输入源即可在 Mac 上直接使用 Windows 应用、安卓模拟器（如 MuMu）或 Steam 游戏。

---

## 核心特性

- **无缝多窗口投射**：每个 Windows 窗口对应 macOS 上独立的原生 `NSWindow`，并自动追踪右键菜单、下拉框、Tooltip 和对话框（无边框非抢焦浮层 `NSPanel`）。
- **双采集模式（按窗口可切换）**：
  - **单窗模式（`WINDOW`）**：基于 `Windows.Graphics.Capture (CreateForWindow)`，Windows 上窗口互相遮挡也能同时并排投射。
  - **屏幕裁剪模式（`MONITOR_CROP`）**：基于 `CreateForMonitor` 按目标窗口可见矩形实时裁剪，完美兼容 **MuMu 等跨进程安卓模拟器、Vulkan / OpenGL 渲染窗口与硬件叠加层**，不黑屏。
- **全硬件低延迟视频管线**：Windows 端 D3D11 GPU `BGRA → NV12` + Media Foundation 硬件 H.264 零 B 帧编码，macOS 端 `VideoToolbox` 硬解 → `IOSurface` 零拷贝上屏（同时兼容 `JPEG` / `RAW_BGRA` 调试模式）。
- **本地等比缩放（不打乱 Windows 布局）**：默认仅在 Mac 本地等比缩放画面，不改动 Windows 原窗口尺寸；也支持一键开启「同步缩放 Windows」。
- **原生级输入体验**：
  - 鼠标归一化坐标（规避 Win10/11 7px 隐形边框与 DPI 偏移）与触控板平滑滚动。
  - USB HID 物理键码映射 + 可选 **`Cmd → Ctrl`** 快捷映射（支持 `Cmd+C/V/X/Z/A/W`、点击、缩放）。
  - **macOS 原生中文输入法**：通过 `NSTextInputClient` 在 Mac 完成选词，经 `KEYEVENTF_UNICODE` 直接注入 Windows。
  - 系统光标与自定义 RGBA 光标同步、文本/PNG 剪贴板双向同步。
- **可灵活开关的低延迟内网音频**：基于 Windows WASAPI 系统环回采集 + 16-bit PCM 传输 + macOS `AVAudioEngine` 实时播放，支持在管理窗口和 Mac 顶部菜单栏随时动态开关与调节音量。
- **Windows 应用启动器 & 完整桌面兜底**：
  - 自动扫描 Windows 桌面与开始菜单快捷方式（`.lnk` / `.url`，支持 Steam 游戏与模拟器快捷方式），带图标与搜索；启动后自动监听并接管新弹出的主窗口。
  - 支持将常用 Windows 应用**星标置顶到 macOS 顶部菜单栏**一键启动。
  - 内置 **「查看完整桌面」** 按钮，随时临时操作 Windows 任务栏与系统托盘。

---

## 仓库结构

```text
winbridge/
├── protocol/           # 两端共享的协议定义（单一事实来源）
│   ├── control.proto   # 控制信道 Protobuf v3 Schema
│   └── SPEC.md         # 单端口双 TCP、32 字节媒体帧头、坐标系与输入规范
├── win-host/           # Windows 被控端（Rust：windows-rs、WGC、Media Foundation、WASAPI、Tokio）
│   └── DESIGN.md       # Windows 端架构设计文档
└── mac-client/         # macOS 主控端（Swift：AppKit、VideoToolbox、AVAudioEngine、Network.framework）
```

---

## 快速开始

### 1. Windows 被控端（`win-host`）

**环境要求**：Windows 10（1903 及以上）或 Windows 11（x64），已安装 Rust stable MSVC 工具链、Visual C++ 生成工具与 Windows SDK（构建脚本已内置 `protoc`，无需手动安装）。

请在**已登录的桌面会话**下的 Developer PowerShell 中构建并运行：

```powershell
cd win-host
cargo check --locked
cargo test --locked
cargo build --release --locked

# 查看当前可投射的窗口列表
.\target\release\win-host.exe --list-windows

# 启动服务（默认监听 9876 端口；将 --token 留空则进入开发模式免鉴权）
.\target\release\win-host.exe --bind 0.0.0.0 --port 9876 --token "your-LAN-secret"
```

常用启动参数：
- `--bind <ADDR>` / `--port <PORT>`：监听地址与端口（控制信道与媒体信道共用单端口，默认 `0.0.0.0:9876`，防火墙只需放行这一个端口）。
- `--token <SECRET>`：内网预共享口令。
- `--codec <auto|h264|jpeg|raw>`：编码器策略（默认 `auto`，按客户端优先级协商）。
- `--capture-mode <window|monitor-crop>`：默认采集模式（默认 `window`；macOS 客户端也可为单个窗口单独覆盖切换）。
- `--fps <FPS>` / `--bitrate <KBPS>`：默认帧率上限（默认 `60`）与 H.264 码率（默认 `15000` kbps）。

### 2. macOS 主控端（`mac-client`）

**环境要求**：macOS 14+，Swift 6.0+（仅需 Apple Command Line Tools，**无需安装 Xcode.app**，零第三方依赖）。

```bash
cd mac-client
./scripts/run_tests.sh
./scripts/bundle_app.sh
open build/WinBridge.app
```

开发期间也可以直接通过终端启动：

```bash
cd mac-client
swift run
```

**使用说明**：
1. 打开 `WinBridge.app`，在顶部输入 Windows 主机的局域网 IP、端口（默认 `9876`）和相同 Token，点击**连接**。
2. 在 **「已打开窗口」** Tab 中双击或点**投射**接管任意窗口；遇到 MuMu 模拟器或黑屏游戏时，将该行采集模式切换为 **`屏幕裁剪`**（会自动按进程名记住）。
3. 在 **「启动应用」** Tab 中搜索并启动 Windows 应用，点 **★** 可置顶到 macOS 顶部菜单栏。
4. 需要操作 Windows 任务栏或系统托盘时，点击 **「查看完整桌面」** 即可临时打开主显示器视图。
5. **关闭技巧**：直接点击远端窗口红叉会向 Windows 发送优雅关闭（`WM_CLOSE`）；按住 **`Option` + 点击红叉**（或连点两次红叉）仅在 Mac 端脱离窗口，不关闭 Windows 程序。
6. **音频说明**：勾选 **「开启音频」** 即可实时收听 Windows 声音（需保持 Windows 音频输出设备未静音；显示器切给 Mac 时本身不会从显示器发声）。

---

## 调试与测试

### Windows 桌面探针与显示器切源测试

使用可调整大小的记事本等临时窗口，验证抓帧、中文注入以及**切换显示器输入源给 Mac 后**是否继续正常出帧：

```powershell
$exe = ".\target\release\win-host.exe"
& $exe --list-windows
$windowId = Read-Host "输入窗口列表中的 HWND"
& $exe --test-capture $windowId --frames 30 --save test.png
& $exe --test-input $windowId

# 在 5 秒延迟期间将显示器输入源切到 Mac，验证是否仍能正常抓帧
& $exe --test-capture $windowId --delay-secs 5 --frames 30 --save after-switch.png
```

> **提示**：如果 Windows 使用 DisplayPort（DP）连接显示器，切源给 Mac 时部分显示器会触发热插拔断连导致桌面消失。可改用 HDMI、安装开源虚拟显示器驱动（Virtual Display Driver）或插一个显卡欺骗器解决。

### Windows 端回环测试与 H.264 录流验证

```powershell
# 运行需要真实桌面的集成测试（双窗口、Resize、弹窗、光标、GPU NV12、H.264 关键帧）
cd win-host
cargo test --locked -- --ignored --test-threads=1

# 录制单窗口裸 H.264 流并用 ffplay 播放验证
$windowId = Read-Host "输入动态窗口的 HWND"
cargo run --release --locked --example record_h264 -- --hwnd $windowId --token "your-LAN-secret" --frames 600 --save window.h264
ffplay -f h264 window.h264
```

---

## 安全说明

当前 v0.1 版本的 TCP 协议采用明文传输（包含 Token、输入事件、视频流、音频流与剪贴板内容）。请**仅在可信的本地局域网内**或搭配加密隧道（如 Tailscale / WireGuard）使用，切勿直接将端口暴露到公网。

---

## 相关文档

- 协议 Schema：[protocol/control.proto](protocol/control.proto)
- 协议规范（帧格式 / DPI / 弹窗 / 音频）：[protocol/SPEC.md](protocol/SPEC.md)
- Windows 端开发设计文档：[win-host/DESIGN.md](win-host/DESIGN.md)
