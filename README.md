# WinBridge

[![CI](https://github.com/WormW/winbridge/actions/workflows/ci.yml/badge.svg)](https://github.com/WormW/winbridge/actions/workflows/ci.yml)

面向局域网（LAN）与远程隧道的**无缝单窗口远程操控工具**：将 Windows 上的单个或多个应用窗口以低延迟、端到端 TLS 1.3 加密方式投射到 macOS 上，像原生 macOS 窗口一样浏览、缩放、输入中文与键鼠/手柄操控。

> 典型场景：Windows 主机与 Mac 共用显示器，平时显示器切给 Mac 使用，无需来回切换显示器输入源即可在 Mac 上直接使用 Windows 应用、安卓模拟器（如 MuMu）或 3D PC 游戏。

---

## 系统架构

```mermaid
flowchart LR
    subgraph Mac["macOS Client (Swift · Zero 3rd-Party Deps)"]
        UI["Manager & Native NSWindow / NSPanel<br/>Performance HUD · File Drop · Smart Game Lock"]
        Keychain["macOS Keychain & TOFU Pinning"]
        Discovery["Bonjour Browser<br/>(_winbridge._tcp)"]
        MediaMac["VideoToolbox HEVC / H.264<br/>→ IOSurface · AudioToolbox Opus/PCM"]
        InputMac["Mouse / HID / Chinese IME<br/>GameController.framework"]
    end

    subgraph Win["Windows Host (Rust · Single Port :9876)"]
        Svc["Session 0 Windows Service<br/>(LocalSystem · ACL service.conf)"]
        Worker["Session 1 User Worker (--session-worker)<br/>WGC · D3D11 · MF HEVC/H.264 · WASAPI+Opus · mDNS"]
        Helper["Session 1 Secure Helper (--secure-helper)<br/>LocalSystem + TokenUIAccess=1<br/>Winlogon UAC/Lock Screen & Anti-Cheat Input"]
    end

    UI <-->|"TLS 1.3 Control & Media (:9876)"| Worker
    Keychain -.->|"SHA-256 TOFU Verify"| Worker
    Discovery <-.->|"mDNS LAN Auto-Discovery"| Worker
    Svc -->|"Anonymous Stdin Pipe<br/>(Token + TLS Key, never in CLI)"| Worker
    Worker <-->|"ACL + GetNamedPipeClientProcessId<br/>Verified Named Pipe"| Helper
    MediaMac -.-> UI
    InputMac -.-> UI
```

---

## 核心特性

### 1. 安全加固（Security Hardening）
- **TLS 1.3 端到端加密 + TOFU 指纹锁定**：默认使用 `rustls`（`ring`）与 macOS `Network.framework` 对控制与媒体双通道进行 TLS 1.3 加密。首次连接自动记录主机证书 SHA-256 指纹（TOFU）；若证书发生意外变化则阻断连接并弹窗警告（支持调试模式 `--no-tls`）。
- **零明文凭据暴露**：
  - Windows 服务模式将口令与配置存储在仅限 `SYSTEM` 与 `Administrators` 可读的 `C:\ProgramData\WinBridge\service.conf`（SDDL 强隔离），SCM `ImagePath` 与子进程命令行中**不再出现任何 Token 或私钥**（由 Session 0 通过匿名 `stdin` 管道传递给 Worker）。
  - macOS 客户端将 `authToken` 自动迁移并加密保存在原生 **macOS Keychain** 中。
- **权限面最小化与防爆破**：
  - `--secure-helper` 改用 **Windows Named Pipe**（禁用远端访问 `PIPE_REJECT_REMOTE_CLIENTS`、严格 SDDL DACL）并在每次连接后通过 **`GetNamedPipeClientProcessId`** 校验调用方必须是服务启动的精确 `--session-worker` PID。
  - 非回环监听默认强制要求 Token（除非显式 `--allow-insecure-no-auth`）；使用常量时间比较、按 IP 指数退避防爆破锁定，并支持 `--allow-cidr` 来源网段白名单。

### 2. 无缝多窗口、游戏操控与日常体验
- **局域网自动发现（mDNS / Bonjour）+ 指数退避自动重连**：Windows 端自动广播 `_winbridge._tcp.local.`，Mac 端下拉即可一键选择主机；意外断连或 Windows 唤醒后自动重连并恢复此前打开的窗口与桌面。
- **双采集模式（按进程记忆）**：
  - **单窗模式（`WINDOW`）**：`WGC CreateForWindow`，支持多窗口互相遮挡并行投射。
  - **屏幕裁剪模式（`MONITOR_CROP`）**：`CreateForMonitor` 裁剪窗口可见边界，完美兼容 MuMu 模拟器、Vulkan / DirectX 游戏与叠加层。
- **智能 3D 游戏鼠标锁定（`⇧⌘L` 开关 / `Ctrl+Option` 随时释放）**：
  - 自动感知 Windows 游戏指针显隐（如游戏内按住 `Alt` 或弹出菜单时自动切回限域可见指针，返回 3D 战斗视角自动切换隐藏相对视角 `dx/dy`）。
  - 支持按进程记住灵敏度倍率与自动开启游戏锁定；可选「背景禁悬停」避免多窗误触。
- **可切换性能 HUD（`⇧⌘H`）**：远端窗口右上角半透明浮层实时展示编码格式、FPS、实时码率（Mbps）、RTT 与分辨率（完全鼠标穿透）。
- **跨设备拖放传文件（双向）**：把 Mac 文件拖入任意远端窗口，分块传到 Windows `Downloads\WinBridge\`。菜单「从 Windows 取回文件…」可以浏览该目录以及下载、桌面、文档（拒绝目录穿越），把文件取回到 Mac 的 `Downloads/WinBridge`，也可以取回 Windows 剪贴板里的文件。单文件上限 512 MiB。
- **手柄透传（可选）**：macOS 原生 `GameController.framework`（Xbox / DualSense / Switch Pro）转发至 Windows；若本机已安装 [ViGEmBus](https://github.com/nefarius/ViGEmBus) 并将 `ViGEmClient.dll` 置于同目录，自动虚拟 Xbox 360 手柄。
- **固定本地代码签名脚本**：提供 `./scripts/setup_codesign.sh`，重新打包 `WinBridge.app` 后保持签名身份不变，彻底避免 macOS 15/26 反复重置「本地网络」与钥匙串权限。

### 3. 视频与音频编解码（4K / 高刷 / Wi-Fi 友好）
- **HEVC (H.265) + H.264 硬件低延迟管线**：Windows D3D11 GPU `BGRA → NV12` + Media Foundation 零 B 帧硬件编码（自动探测并优先协商 `HEVC → H.264 → JPEG`），macOS `VideoToolbox` 硬解 → `IOSurface` 零拷贝上屏。
- **Opus 低带宽音频 + PCM 无损双模**：基于 Windows WASAPI 环回采集 + 纯 Rust RFC 6716 **Opus 编码**（内置 48 kHz 重采样，支持 `Opus 128k` / `Opus 64k` 将带宽降至原 PCM 的 1/12 ~ 1/24）与 macOS 原生 `AudioToolbox`（`AVAudioConverter`）硬/系统解码，亦可一键切回 `PCM 无损`。菜单「只采集已投射进程」会把已订阅窗口的进程树混成一路音频（Windows 10 20348+ / Windows 11）；不支持或订阅了完整桌面时自动退回整机环回，并在 Mac 上提示。

---

## 仓库结构

```text
winbridge/
├── .github/workflows/  # CI（Windows MSVC + macOS Swift 6）与 Tag 自动发布 Release
├── protocol/           # 两端共享的协议定义（单一事实来源）
│   ├── control.proto   # 控制信道 Protobuf v3 Schema
│   └── SPEC.md         # TLS 1.3 + 单端口双信道、32B 媒体头、HEVC/Opus/输入/文件传输规范
├── win-host/           # Windows 被控端（Rust：WGC、Media Foundation HEVC/H.264、WASAPI+Opus、rustls、mDNS）
│   └── DESIGN.md       # Windows 端架构设计文档
└── mac-client/         # macOS 主控端（Swift：AppKit、VideoToolbox、AVAudioEngine/Opus、Network TLS、Keychain、GameController）
```

---

## 快速开始

### 1. Windows 被控端（`win-host`）

**环境要求**：Windows 10（1903+）或 Windows 11（x64），已安装 Rust stable MSVC 工具链与 Visual C++ 生成工具（无需手动安装 `protoc` 或 CMake）。

```powershell
cd win-host
cargo test --locked
cargo build --release --locked

# 推荐：以管理员 PowerShell 安装为系统服务（自动生成强随机 Token 并写入受保护的 service.conf，支持 UAC & 锁屏点击）
.\target\release\win-host.exe --install-service --bind 0.0.0.0 --port 9876

# 或显式指定自定义 Token 与允许的来源网段
.\target\release\win-host.exe --install-service --bind 0.0.0.0 --port 9876 --token "your-LAN-secret" --allow-cidr 192.168.1.0/24

# 前台直接运行（开发/单次调试；亦可通过环境变量 WINBRIDGE_TOKEN 或 --token-file 传入口令）
$env:WINBRIDGE_TOKEN = "your-LAN-secret"
.\target\release\win-host.exe --bind 0.0.0.0 --port 9876

# 停止并卸载系统服务
.\target\release\win-host.exe --uninstall-service
```

常用启动参数：
- `--bind <ADDR>` / `--port <PORT>`：监听地址与单端口（默认 `0.0.0.0:9876`）。
- `--token <SECRET>` / `--token-file <PATH>` / 环境变量 `WINBRIDGE_TOKEN`：预共享鉴权口令（非回环监听默认必填；仅受控测试可用 `--allow-insecure-no-auth`）。
- `--allow-cidr <CIDR>`：可重复指定的来源 IP/CIDR 白名单（如 `192.168.1.0/24`、`100.64.0.0/10`）。
- `--no-tls` / `--tls-dir <DIR>`：禁用 TLS（仅调试）或指定自定义 TLS 证书目录。
- `--no-discovery`：禁用局域网 mDNS 广播。
- `--codec <auto|hevc|h264|jpeg|raw>`：编码器策略（默认 `auto`，优先硬件 HEVC → H.264 → JPEG）。
- `--capture-mode <window|monitor-crop>` / `--fps <FPS>` / `--bitrate <KBPS>`：默认采集模式、帧率上限（默认 `60`）与视频码率（默认 `15000` kbps）。

### 2. macOS 主控端（`mac-client`）

**环境要求**：macOS 14+，Swift 6.0+（仅需 Apple Command Line Tools，无需安装 Xcode.app，零第三方依赖）。

```bash
cd mac-client
./scripts/run_tests.sh

# 可选（强烈推荐，仅需执行一次）：创建本地固定自签证书，防止重打包后 macOS 重置本地网络权限
./scripts/setup_codesign.sh

./scripts/bundle_app.sh
open build/WinBridge.app
```

**快捷使用指南**：
1. 打开 `WinBridge.app`，从顶部 **「发现主机」** 下拉框一键选择局域网内的 Windows 主机（或手动输入 IP），粘贴 Token 点击**连接**。
2. 首次 TLS 连接会自动信任并锁定 Windows 主机 SHA-256 证书指纹（可在菜单 `窗口 → 重置当前主机 TLS 证书信任` 重置）。
3. **快捷键一览**：
   - `⇧⌘L`：开启 / 关闭智能 3D 游戏鼠标锁定（`Ctrl+Option` 随时紧急释放）
   - `⇧⌘H`：显示 / 隐藏右上角实时性能 HUD（FPS / Mbps / RTT / 分辨率）
   - `⌘,`：唤出 WinBridge 管理窗口；`Option + 点红叉`（或 2 秒内连点两次）：仅在 Mac 端脱离而不关闭 Windows 软件
   - 拖放 Mac 文件到任意远端窗口：自动发送至 Windows `Downloads\WinBridge\`

---

## 调试与测试

```powershell
# Windows 桌面集成测试（双窗口、Resize、弹窗、光标、GPU NV12、H.264 关键帧）
cd win-host
cargo test --locked -- --ignored --test-threads=1
```

---

## 相关文档

- 协议 Schema：[protocol/control.proto](protocol/control.proto)
- 协议规范（TLS / 帧格式 / HEVC / Opus / 文件传输 / 手柄）：[protocol/SPEC.md](protocol/SPEC.md)
- Windows 端开发设计文档：[win-host/DESIGN.md](win-host/DESIGN.md)
