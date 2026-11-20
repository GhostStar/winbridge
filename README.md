# WinBridge

LAN-first seamless remote window controller: display and operate individual Windows application windows natively on macOS.

## Planned Repository Layout

```text
winbridge/
├── protocol/           # Single source of truth shared by Windows & macOS
│   ├── control.proto   # Protobuf schema for the Control TCP channel
│   └── SPEC.md         # Transport, 32-byte media frame header, DPI/input contracts
├── win-host/           # (Next) Rust Windows host (windows-rs, tokio, prost)
└── mac-client/         # (Next) Swift macOS client (AppKit, VideoToolbox, Network.framework)
```

## Codegen Hints

- **Windows (`win-host` / Rust)**: Use [`prost-build`](https://docs.rs/prost-build) in `build.rs` pointing at `../protocol/control.proto`.
- **macOS (`mac-client` / Swift)**: Use [`swift-protobuf`](https://github.com/apple/swift-protobuf) (`protoc --swift_out=... protocol/control.proto`).
