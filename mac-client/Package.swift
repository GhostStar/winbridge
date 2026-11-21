// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "WinBridgeClient",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .executable(name: "WinBridgeClient", targets: ["WinBridgeClient"]),
    ],
    targets: [
        .target(
            name: "WinBridgeCore",
            path: "Sources/WinBridgeCore",
            linkerSettings: [
                .linkedFramework("AppKit"),
                .linkedFramework("AVFoundation"),
                .linkedFramework("CoreGraphics"),
                .linkedFramework("CoreMedia"),
                .linkedFramework("CoreVideo"),
                .linkedFramework("GameController"),
                .linkedFramework("IOSurface"),
                .linkedFramework("Network"),
                .linkedFramework("QuartzCore"),
                .linkedFramework("Security"),
                .linkedFramework("VideoToolbox"),
                .linkedFramework("ImageIO"),
                .linkedFramework("Carbon"),
            ]
        ),
        .executableTarget(
            name: "WinBridgeClient",
            dependencies: ["WinBridgeCore"],
            path: "Sources/WinBridgeClient"
        ),
        .testTarget(
            name: "WinBridgeClientTests",
            dependencies: ["WinBridgeCore"],
            path: "Tests/WinBridgeClientTests"
        ),
    ],
    swiftLanguageModes: [.v5]
)
