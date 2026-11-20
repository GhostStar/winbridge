#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
APP_DIR="$ROOT_DIR/build/WinBridge.app"

cd "$ROOT_DIR"
EXTRA_ARGS=()
if [[ "${1:-}" == "--disable-sandbox" ]]; then
    EXTRA_ARGS+=(--disable-sandbox)
fi
swift build -c release "${EXTRA_ARGS[@]}"

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"

cp "$ROOT_DIR/.build/release/WinBridgeClient" "$APP_DIR/Contents/MacOS/WinBridge"

cat > "$APP_DIR/Contents/Info.plist" << 'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>zh_CN</string>
    <key>CFBundleExecutable</key>
    <string>WinBridge</string>
    <key>CFBundleIdentifier</key>
    <string>com.winbridge.client</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>WinBridge</string>
    <key>CFBundleDisplayName</key>
    <string>WinBridge</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSMinimumSystemVersion</key>
    <string>14.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSLocalNetworkUsageDescription</key>
    <string>WinBridge 需要访问局域网以连接您的 Windows 主机并投射应用窗口。</string>
</dict>
</plist>
EOF

echo "Built app bundle at: $APP_DIR"
echo "Run with: open \"$APP_DIR\""
