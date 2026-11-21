import AppKit
import CoreGraphics
import Foundation

enum CursorMapper {
    private static let hiddenCursor: NSCursor = {
        let img = NSImage(size: NSSize(width: 1, height: 1))
        return NSCursor(image: img, hotSpot: .zero)
    }()

    /// Returns true when Windows has hidden the cursor (or set an all-transparent custom cursor),
    /// which 3D games do during camera rotation and clear when showing menus or holding Alt.
    static func isEffectivelyHidden(_ shape: CursorShape) -> Bool {
        if shape.cursorType == .hidden {
            return true
        }
        if shape.cursorType == .custom {
            let bytes = [UInt8](shape.customRgba)
            guard bytes.count >= 4 else { return false }
            var i = 3
            while i < bytes.count {
                if bytes[i] != 0 {
                    return false
                }
                i += 4
            }
            return true
        }
        return false
    }

    static func cursor(from shape: CursorShape) -> NSCursor {
        switch shape.cursorType {
        case .unspecified, .arrow:
            return .arrow
        case .ibeam:
            return .iBeam
        case .hand:
            return .pointingHand
        case .crosshair:
            return .crosshair
        case .wait, .appStarting:
            return .arrow
        case .sizeNS:
            return .resizeUpDown
        case .sizeWE:
            return .resizeLeftRight
        case .sizeNWSE, .sizeNESW:
            // Standard NSCursor lacks public diagonal arrows; fall back cleanly to crosshair/openHand or arrow.
            return .crosshair
        case .sizeAll:
            return .openHand
        case .no:
            return .operationNotAllowed
        case .hidden:
            return hiddenCursor
        case .custom:
            if let custom = buildCustomCursor(shape) {
                return custom
            }
            return .arrow
        }
    }

    private static func buildCustomCursor(_ shape: CursorShape) -> NSCursor? {
        let w = Int(shape.width)
        let h = Int(shape.height)
        guard w > 0, h > 0, w <= 256, h <= 256 else { return nil }
        let expectedBytes = w * h * 4
        guard shape.customRgba.count >= expectedBytes else { return nil }
        guard let provider = CGDataProvider(data: shape.customRgba as CFData) else { return nil }
        let bitmapInfo = CGBitmapInfo(rawValue: CGBitmapInfo.byteOrder32Big.rawValue | CGImageAlphaInfo.last.rawValue)
        guard let cgImage = CGImage(
            width: w,
            height: h,
            bitsPerComponent: 8,
            bitsPerPixel: 32,
            bytesPerRow: w * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: bitmapInfo,
            provider: provider,
            decode: nil,
            shouldInterpolate: true,
            intent: .defaultIntent
        ) else {
            return nil
        }

        // Scale down standard 32px+ high-DPI cursors slightly if oversized, preserving hotspot ratio.
        let scale: CGFloat = (w >= 48) ? 2.0 : 1.0
        let size = NSSize(width: CGFloat(w) / scale, height: CGFloat(h) / scale)
        let image = NSImage(cgImage: cgImage, size: size)
        let hotSpot = NSPoint(
            x: min(size.width - 1, max(0, CGFloat(shape.hotspotX) / scale)),
            y: min(size.height - 1, max(0, CGFloat(shape.hotspotY) / scale))
        )
        return NSCursor(image: image, hotSpot: hotSpot)
    }
}
