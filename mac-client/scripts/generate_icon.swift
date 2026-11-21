#!/usr/bin/env swift
import AppKit
import CoreGraphics
import Foundation
import QuartzCore

/// Generates WinBridge's native macOS Sequoia Squircle icon (Dark Obsidian + Cyan Bridge)
/// as a 1024x1024 master PNG and a full multi-resolution AppIcon.icns via `iconutil`.

let scriptURL = URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL
let macClientRoot = scriptURL.deletingLastPathComponent().deletingLastPathComponent()
let resourcesDir = macClientRoot.appendingPathComponent("Resources", isDirectory: true)
let iconsetDir = resourcesDir.appendingPathComponent("AppIcon.iconset", isDirectory: true)
let icnsURL = resourcesDir.appendingPathComponent("AppIcon.icns")
let masterPNGURL = resourcesDir.appendingPathComponent("AppIcon-1024.png")

func pngData(_ image: CGImage) throws -> Data {
    let rep = NSBitmapImageRep(cgImage: image)
    guard let data = rep.representation(using: .png, properties: [:]) else {
        throw NSError(domain: "WinBridgeIcon", code: 1, userInfo: [NSLocalizedDescriptionKey: "Failed to encode PNG"])
    }
    return data
}

func savePNG(_ image: CGImage, to url: URL) throws {
    try pngData(image).write(to: url)
}

func makeDarkVectorArtwork() throws -> CGImage {
    let size = 1024
    let cs = CGColorSpaceCreateDeviceRGB()
    guard let ctx = CGContext(
        data: nil,
        width: size,
        height: size,
        bitsPerComponent: 8,
        bytesPerRow: 0,
        space: cs,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else {
        throw NSError(domain: "WinBridgeIcon", code: 2)
    }

    ctx.setAllowsAntialiasing(true)
    ctx.setShouldAntialias(true)

    // 1. Anodized Obsidian Slate gradient backdrop
    let bgColors = [
        CGColor(red: 0.15, green: 0.17, blue: 0.24, alpha: 1),
        CGColor(red: 0.065, green: 0.075, blue: 0.115, alpha: 1),
    ] as CFArray
    if let bgGrad = CGGradient(colorsSpace: cs, colors: bgColors, locations: [0, 1]) {
        ctx.drawLinearGradient(bgGrad, start: CGPoint(x: 0, y: 1024), end: CGPoint(x: 1024, y: 0), options: [])
    }

    // 2. Ambient center halo
    let glowColors = [
        CGColor(red: 0.12, green: 0.72, blue: 1.0, alpha: 0.26),
        CGColor(red: 0.36, green: 0.32, blue: 0.96, alpha: 0.10),
        CGColor(red: 0.0, green: 0.0, blue: 0.0, alpha: 0.0),
    ] as CFArray
    if let glow = CGGradient(colorsSpace: cs, colors: glowColors, locations: [0, 0.52, 1]) {
        ctx.drawRadialGradient(
            glow,
            startCenter: CGPoint(x: 460, y: 540),
            startRadius: 10,
            endCenter: CGPoint(x: 512, y: 512),
            endRadius: 460,
            options: [.drawsBeforeStartLocation]
        )
    }

    // 3. Back Card: Luminous 4-pane Windows prism tile (top-left)
    let winCard = CGRect(x: 170, y: 420, width: 390, height: 390)
    let winCardPath = CGPath(roundedRect: winCard, cornerWidth: 52, cornerHeight: 52, transform: nil)
    ctx.saveGState()
    ctx.setShadow(
        offset: CGSize(width: 0, height: -12),
        blur: 40,
        color: CGColor(red: 0.10, green: 0.78, blue: 1.0, alpha: 0.45)
    )
    ctx.addPath(winCardPath)
    ctx.setFillColor(CGColor(red: 0.15, green: 0.62, blue: 0.98, alpha: 1.0))
    ctx.fillPath()
    ctx.restoreGState()

    ctx.saveGState()
    ctx.addPath(winCardPath)
    ctx.clip()
    let winBg = [
        CGColor(red: 0.26, green: 0.88, blue: 1.0, alpha: 1.0),
        CGColor(red: 0.10, green: 0.44, blue: 0.96, alpha: 1.0),
    ] as CFArray
    if let g = CGGradient(colorsSpace: cs, colors: winBg, locations: [0, 1]) {
        ctx.drawLinearGradient(
            g,
            start: CGPoint(x: winCard.minX, y: winCard.maxY),
            end: CGPoint(x: winCard.maxX, y: winCard.minY),
            options: []
        )
    }
    let pad: CGFloat = 28
    let gap: CGFloat = 22
    let tileSize = (winCard.width - pad * 2 - gap) / 2
    for row in 0..<2 {
        for col in 0..<2 {
            let tx = winCard.minX + pad + CGFloat(col) * (tileSize + gap)
            let ty = winCard.minY + pad + CGFloat(row) * (tileSize + gap)
            let tile = CGRect(x: tx, y: ty, width: tileSize, height: tileSize)
            let tp = CGPath(roundedRect: tile, cornerWidth: 24, cornerHeight: 24, transform: nil)
            ctx.addPath(tp)
            ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 0.28))
            ctx.fillPath()
            ctx.addPath(tp)
            ctx.setStrokeColor(CGColor(red: 1, green: 1, blue: 1, alpha: 0.50))
            ctx.setLineWidth(3.0)
            ctx.strokePath()
        }
    }
    ctx.restoreGState()

    // 4. Front Card: Sleek dark-glass macOS window (bottom-right)
    let macCard = CGRect(x: 410, y: 215, width: 445, height: 355)
    let macPath = CGPath(roundedRect: macCard, cornerWidth: 40, cornerHeight: 40, transform: nil)
    ctx.saveGState()
    ctx.setShadow(
        offset: CGSize(width: 0, height: -18),
        blur: 44,
        color: CGColor(red: 0, green: 0, blue: 0, alpha: 0.65)
    )
    ctx.addPath(macPath)
    ctx.setFillColor(CGColor(red: 0.14, green: 0.17, blue: 0.24, alpha: 1.0))
    ctx.fillPath()
    ctx.restoreGState()

    ctx.saveGState()
    ctx.addPath(macPath)
    ctx.clip()
    let macGradColors = [
        CGColor(red: 0.23, green: 0.27, blue: 0.37, alpha: 1.0),
        CGColor(red: 0.11, green: 0.13, blue: 0.20, alpha: 1.0),
    ] as CFArray
    if let mg = CGGradient(colorsSpace: cs, colors: macGradColors, locations: [0, 1]) {
        ctx.drawLinearGradient(
            mg,
            start: CGPoint(x: macCard.minX, y: macCard.maxY),
            end: CGPoint(x: macCard.maxX, y: macCard.minY),
            options: []
        )
    }
    let spillColors = [
        CGColor(red: 0.26, green: 0.88, blue: 1.0, alpha: 0.32),
        CGColor(red: 0.26, green: 0.88, blue: 1.0, alpha: 0.0),
    ] as CFArray
    if let sg = CGGradient(colorsSpace: cs, colors: spillColors, locations: [0, 1]) {
        ctx.drawRadialGradient(
            sg,
            startCenter: CGPoint(x: macCard.minX + 75, y: macCard.maxY - 75),
            startRadius: 8,
            endCenter: CGPoint(x: macCard.minX + 75, y: macCard.maxY - 75),
            endRadius: 210,
            options: [.drawsBeforeStartLocation]
        )
    }
    let barRect = CGRect(x: macCard.minX, y: macCard.maxY - 66, width: macCard.width, height: 66)
    ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 0.09))
    ctx.fill(barRect)
    ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 0.10))
    ctx.fill(CGRect(x: macCard.minX, y: macCard.maxY - 66, width: macCard.width, height: 1.5))

    let dotColors = [
        CGColor(red: 1.0, green: 0.37, blue: 0.34, alpha: 1.0),
        CGColor(red: 1.0, green: 0.74, blue: 0.18, alpha: 1.0),
        CGColor(red: 0.16, green: 0.79, blue: 0.26, alpha: 1.0),
    ]
    for (idx, col) in dotColors.enumerated() {
        let cx = macCard.minX + 40 + CGFloat(idx) * 32
        let cy = macCard.maxY - 33
        ctx.setFillColor(col)
        ctx.fillEllipse(in: CGRect(x: cx - 9.5, y: cy - 9.5, width: 19, height: 19))
    }
    ctx.restoreGState()

    ctx.addPath(macPath)
    ctx.setStrokeColor(CGColor(red: 1, green: 1, blue: 1, alpha: 0.34))
    ctx.setLineWidth(3.5)
    ctx.strokePath()

    // 5. Luminous Cyan-White Bridge Arc + glowing endpoints
    let bridgePath = CGMutablePath()
    bridgePath.move(to: CGPoint(x: 270, y: 505))
    bridgePath.addCurve(
        to: CGPoint(x: 735, y: 390),
        control1: CGPoint(x: 395, y: 745),
        control2: CGPoint(x: 665, y: 690)
    )

    ctx.saveGState()
    ctx.setShadow(offset: .zero, blur: 32, color: CGColor(red: 0.22, green: 0.90, blue: 1.0, alpha: 0.95))
    ctx.addPath(bridgePath)
    ctx.setStrokeColor(CGColor(red: 0.35, green: 0.95, blue: 1.0, alpha: 0.90))
    ctx.setLineCap(.round)
    ctx.setLineWidth(18)
    ctx.strokePath()
    ctx.restoreGState()

    ctx.saveGState()
    ctx.addPath(bridgePath)
    ctx.setStrokeColor(CGColor(red: 0.86, green: 0.99, blue: 1.0, alpha: 0.98))
    ctx.setLineCap(.round)
    ctx.setLineWidth(7)
    ctx.strokePath()
    ctx.restoreGState()

    for pt in [CGPoint(x: 270, y: 505), CGPoint(x: 735, y: 390)] {
        ctx.saveGState()
        ctx.setShadow(offset: .zero, blur: 18, color: CGColor(red: 0.30, green: 0.94, blue: 1.0, alpha: 1.0))
        ctx.setFillColor(CGColor(red: 0.90, green: 0.99, blue: 1.0, alpha: 1.0))
        ctx.fillEllipse(in: CGRect(x: pt.x - 12, y: pt.y - 12, width: 24, height: 24))
        ctx.restoreGState()
    }

    guard let out = ctx.makeImage() else { throw NSError(domain: "WinBridgeIcon", code: 3) }
    return out
}

func renderAppleSquircle(artwork: CGImage) throws -> CGImage {
    let size = 1024
    let cs = CGColorSpaceCreateDeviceRGB()
    guard let ctx = CGContext(
        data: nil,
        width: size,
        height: size,
        bitsPerComponent: 8,
        bytesPerRow: 0,
        space: cs,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else {
        throw NSError(domain: "WinBridgeIcon", code: 4)
    }

    let rootLayer = CALayer()
    rootLayer.frame = CGRect(x: 0, y: 0, width: size, height: size)
    rootLayer.backgroundColor = CGColor.clear

    let iconFrame = CGRect(x: 100, y: 100, width: 824, height: 824)
    let shadowLayer = CALayer()
    shadowLayer.frame = iconFrame
    shadowLayer.backgroundColor = CGColor(red: 0.04, green: 0.06, blue: 0.12, alpha: 1.0)
    shadowLayer.cornerRadius = 185.4
    shadowLayer.cornerCurve = .continuous
    shadowLayer.shadowColor = CGColor(red: 0, green: 0, blue: 0, alpha: 1.0)
    shadowLayer.shadowOpacity = 0.34
    shadowLayer.shadowOffset = CGSize(width: 0, height: -14)
    shadowLayer.shadowRadius = 22
    rootLayer.addSublayer(shadowLayer)

    let maskLayer = CALayer()
    maskLayer.frame = iconFrame
    maskLayer.cornerRadius = 185.4
    maskLayer.cornerCurve = .continuous
    maskLayer.masksToBounds = true
    maskLayer.contents = artwork
    maskLayer.contentsGravity = .resizeAspectFill
    maskLayer.borderWidth = 2.0
    maskLayer.borderColor = CGColor(red: 1, green: 1, blue: 1, alpha: 0.16)
    rootLayer.addSublayer(maskLayer)

    rootLayer.render(in: ctx)
    guard let out = ctx.makeImage() else { throw NSError(domain: "WinBridgeIcon", code: 5) }
    return out
}

func resizedImage(_ source: CGImage, pixels: Int) throws -> CGImage {
    let cs = CGColorSpaceCreateDeviceRGB()
    guard let ctx = CGContext(
        data: nil,
        width: pixels,
        height: pixels,
        bitsPerComponent: 8,
        bytesPerRow: 0,
        space: cs,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else {
        throw NSError(domain: "WinBridgeIcon", code: 6)
    }
    ctx.interpolationQuality = .high
    ctx.setAllowsAntialiasing(true)
    ctx.setShouldAntialias(true)
    ctx.draw(source, in: CGRect(x: 0, y: 0, width: pixels, height: pixels))
    guard let out = ctx.makeImage() else { throw NSError(domain: "WinBridgeIcon", code: 7) }
    return out
}

let fm = FileManager.default
try fm.createDirectory(at: resourcesDir, withIntermediateDirectories: true)
if fm.fileExists(atPath: iconsetDir.path) {
    try fm.removeItem(at: iconsetDir)
}

let master = try renderAppleSquircle(artwork: try makeDarkVectorArtwork())
try savePNG(master, to: masterPNGURL)

// Native Apple ICNS container with full 1x & @2x Retina PNG representations (16px..1024px).
let icnsEntries: [(String, Int)] = [
    ("icp4", 16),   // 16x16
    ("ic11", 32),   // 16x16@2x
    ("icp5", 32),   // 32x32
    ("ic12", 64),   // 32x32@2x
    ("ic07", 128),  // 128x128
    ("ic13", 256),  // 128x128@2x
    ("ic08", 256),  // 256x256
    ("ic14", 512),  // 256x256@2x
    ("ic09", 512),  // 512x512
    ("ic10", 1024), // 512x512@2x
]

var pngCache: [Int: Data] = [:]
var body = Data()
for (ostype, px) in icnsEntries {
    let data: Data
    if let cached = pngCache[px] {
        data = cached
    } else {
        let img = px == 1024 ? master : try resizedImage(master, pixels: px)
        let encoded = try pngData(img)
        pngCache[px] = encoded
        data = encoded
    }
    body.append(contentsOf: ostype.utf8)
    var chunkLen = UInt32(8 + data.count).bigEndian
    withUnsafeBytes(of: &chunkLen) { body.append(contentsOf: $0) }
    body.append(data)
}

var icns = Data()
icns.append(contentsOf: "icns".utf8)
var totalLen = UInt32(8 + body.count).bigEndian
withUnsafeBytes(of: &totalLen) { icns.append(contentsOf: $0) }
icns.append(body)
try icns.write(to: icnsURL)

print("Generated \(icnsURL.path) (\(icns.count) bytes) and \(masterPNGURL.path)")
