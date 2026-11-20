# WinBridge

LAN-first seamless remote window controller: display and operate individual Windows application windows natively on macOS.

## Repository Layout

```text
winbridge/
├── protocol/           # Single source of truth shared by Windows & macOS
│   ├── control.proto   # Protobuf schema for the Control TCP channel
│   └── SPEC.md         # Transport, 32-byte media frame header, DPI/input contracts
├── win-host/           # Rust Windows host (Windows.Graphics.Capture, Media Foundation, Tokio)
└── mac-client/         # Swift macOS client (AppKit, VideoToolbox, Network.framework)
```

## macOS Client

Built with Swift Package Manager (Swift 6.0+, macOS 14+, no Xcode.app required, zero third-party dependencies):

```bash
cd mac-client
./scripts/run_tests.sh
./scripts/bundle_app.sh
open build/WinBridge.app
```

Or run directly from the terminal during development:

```bash
cd mac-client
swift run
```

Features:
- Dual TCP transport (`WBRGCTL\x01` + `WBRGMED\x01`, `TCP_NODELAY`, 5s `Ping` keepalive & live RTT)
- Hardware H.264 decoding via `VideoToolbox` → `IOSurface` → `CALayer` (plus `JPEG` and `RAW_BGRA`)
- Seamless multi-window rendering (`NORMAL`, `DIALOG`, and non-activating borderless `POPUP` menus/tooltips)
- Mouse, smooth trackpad scrolling, USB HID keyboard mapping, optional `Cmd → Ctrl`, and native macOS Chinese IME (`NSTextInputClient` → `TextInput`)
- Cursor shape sync (standard + custom RGBA) and bidirectional text/PNG clipboard sync
- Optional real-time LAN system audio streaming (Windows WASAPI loopback -> 16-bit PCM -> macOS `AVAudioEngine`) with live toggle and volume slider
- Built-in Windows Start Menu & Desktop app launcher with search, Menu Bar pinning, automatic new-window takeover, and a one-click Full Windows Desktop fallback view

## Windows Host

Build in an x64 Developer PowerShell with the Rust stable MSVC toolchain, the Visual C++ build tools and a Windows SDK. Run the host in the signed-in desktop session on Windows 10 (1903 or newer) or Windows 11. The build includes protoc.

```powershell
cd win-host
cargo check --locked
cargo test --locked
cargo build --release --locked
.\target\release\win-host.exe --list-windows
.\target\release\win-host.exe --bind 0.0.0.0 --port 9876 --token "your-LAN-secret"
```

Use `--capture-mode monitor-crop` to default to monitor capture cropped to the window's visible bounds (useful for Vulkan / cross-process emulators like MuMu), or let the macOS client pick `单窗` vs `屏幕裁剪` per window via `SubscribeWindow.capture_mode`.

Enter the Windows host's LAN address, port and matching token on the client. An empty token enables development mode. The v0.1 TCP transport carries the token, input, pixels and clipboard contents in cleartext. Use a trusted LAN or an encrypted tunnel.

### Desktop diagnostics

Use a disposable, resizable text-editor window for the input probe. It focuses the selected window, types mixed Chinese and English text, and changes its size.

```powershell
$exe = ".\target\release\win-host.exe"
& $exe --list-windows
$windowId = Read-Host "HWND from the window list"
& $exe --test-capture $windowId --frames 30 --save test.png
& $exe --test-input $windowId
& $exe --test-capture $windowId --delay-secs 5 --frames 30 --save after-switch.png
```

For the last command, change the monitor input during the delay. Inspect the saved frame and the reported size. An animated target provides a continuous source of WGC frames.

### Loopback and H.264 verification

The regular test suite starts a private loopback host and checks authentication, channel binding and teardown. Run the desktop tests in an unlocked session to exercise two real windows, visible resize, popup and cursor events, the GPU conversion path, and H.264 keyframes.

```powershell
cargo test --locked -- --ignored --test-threads=1
```

To inspect a stream, start the host in one terminal. In a second terminal, use the HWND of an animated window and the same token. The example records the Annex-B payloads received over TCP and prints the measured frame rate.

```powershell
$windowId = Read-Host "HWND from the window list"
cargo run --release --locked --example record_h264 -- --hwnd $windowId --token "your-LAN-secret" --frames 600 --save window.h264
ffplay -f h264 window.h264
```

Use `--codec auto` for client-ordered negotiation, or select `h264`, `jpeg` or `raw` explicitly. An H.264 keyframe includes the decoder parameter sets. With `--log-level debug`, the host also reports capture and encoder processing time.

## Codegen Hints

- **Windows (`win-host` / Rust)**: Use [`prost-build`](https://docs.rs/prost-build) in `build.rs` pointing at `../protocol/control.proto`.
- **macOS (`mac-client` / Swift)**: Use [`swift-protobuf`](https://github.com/apple/swift-protobuf) (`protoc --swift_out=... protocol/control.proto`).
