import AppKit
import Testing
@testable import WinBridgeCore

@Test func borderlessPanelMetadataPreservesLegacyAndModalChrome() throws {
    let card = WindowInfo(windowId: 2, ownerWindowId: 1, kind: .dialog, isBorderlessPanel: true)
    #expect(try WindowInfo.decode(from: card.encode()) == card)
    #expect(card.usesBorderlessPresentation)
    #expect(!WindowInfo(kind: .dialog).usesBorderlessPresentation) // old host
    #expect(!WindowInfo(kind: .normal, hasAlpha: true, isBorderlessPanel: true).usesBorderlessPresentation)
    #expect(!WindowInfo(kind: .dialog, isModal: true, isBorderlessPanel: true).usesBorderlessPresentation)
}

@MainActor @Test func contactCardHasNoChromeButKeepsFocusAndOwnerGeometry() throws {
    _ = NSApplication.shared
    let root = RemoteWindowController(info: WindowInfo(windowId: 1, kind: .normal,
        width: 800, height: 600), owner: nil)
    let card = RemoteWindowController(info: WindowInfo(windowId: 2, ownerWindowId: 1,
        kind: .dialog, width: 300, height: 200, offsetX: 25, offsetY: 35,
        isBorderlessPanel: true), owner: root)
    defer { card.forceCloseLocally(); root.forceCloseLocally() }
    root.present(owner: nil); card.present(owner: root)
    let window = try #require(card.window)
    #expect(!window.styleMask.contains(.titled))
    #expect(!window.styleMask.contains(.resizable))
    #expect(window.standardWindowButton(.closeButton) == nil)
    #expect(window.canBecomeKey && !window.canBecomeMain)
    #expect(window.parent === root.window)
    #expect(window.isExcludedFromWindowsMenu)
    #expect(card.remoteView.inputEnabled)
    #expect(abs(window.frame.minX - (root.videoRectInScreen.minX + 25)) < 1)
    #expect(abs(window.frame.maxY - (root.videoRectInScreen.maxY - 35)) < 1)
    #expect(window.frame.size == window.contentRect(forFrameRect: window.frame).size)
    // No alpha is required for borderless presentation; opaque cards stay opaque.
    #expect(window.isOpaque)
    card.applyTransparency(true)
    #expect(!window.isOpaque && !window.hasShadow)
}

@MainActor @Test func cardChromeUpdatesInPlaceWithoutMovingContentOrSendingRemoteActions() throws {
    _ = NSApplication.shared
    var info = WindowInfo(windowId: 1, kind: .dialog, width: 300, height: 200)
    let card = RemoteWindowController(info: info, owner: nil)
    defer { card.forceCloseLocally() }
    let window = try #require(card.window)
    let initial = window.contentRect(forFrameRect: window.frame)
    var sent: [ControlMessage.Payload] = []
    card.onSendControl = { sent.append($0) }
    info.isBorderlessPanel = true
    card.applyUpdate(WindowUpdated(window: info), owner: nil)
    #expect(!window.styleMask.contains(.titled))
    #expect(window.contentRect(forFrameRect: window.frame) == initial)
    info.isModal = true
    card.applyUpdate(WindowUpdated(window: info), owner: nil)
    #expect(window.styleMask.contains(.titled))
    #expect(window.contentRect(forFrameRect: window.frame) == initial)
    #expect(!sent.contains { if case .resizeWindow = $0 { return true }; if case .closeWindow = $0 { return true }; return false })
}

@MainActor @Test func transparentIndependentViewersStillHaveLocalWindowButtons() {
    _ = NSApplication.shared
    let image = RemoteWindowController(info: WindowInfo(windowId: 1, kind: .normal,
        width: 320, height: 240, hasAlpha: true), owner: nil)
    let dialog = RemoteWindowController(info: WindowInfo(windowId: 2, kind: .dialog,
        width: 320, height: 240, isModal: true), owner: nil)
    defer { image.forceCloseLocally(); dialog.forceCloseLocally() }
    #expect(image.window?.styleMask.contains([.titled, .closable, .miniaturizable, .resizable]) == true)
    #expect(dialog.window?.styleMask.contains([.titled, .closable]) == true)
}
