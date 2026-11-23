import AppKit
import Testing
@testable import WinBridgeCore

@Test func localCloseOwnershipExcludesIndependentApplicationMembers() {
    let root = WindowInfo(windowId: 1, kind: .normal)
    let panel = WindowInfo(windowId: 2, ownerWindowId: 1, kind: .dialog)
    let menu = WindowInfo(windowId: 3, ownerWindowId: 2, kind: .popup)
    let image = WindowInfo(windowId: 4, ownerWindowId: 1, kind: .normal)
    let nested = WindowInfo(windowId: 5, ownerWindowId: 4, kind: .dialog)
    let windows = [nested, menu, image, panel, root]
    #expect(ViewerVisibility.subtree(1, in: windows) == [1, 2, 3])
    #expect(ViewerVisibility.subtree(4, in: windows) == [4, 5])
    #expect(ViewerVisibility.ancestors(3, in: windows) == [1, 2, 3])
    #expect(ViewerVisibility.ancestors(5, in: windows) == [4, 5])
    let cycle = [WindowInfo(windowId: 7, ownerWindowId: 8, kind: .dialog),
                 WindowInfo(windowId: 8, ownerWindowId: 7, kind: .dialog)]
    #expect(ViewerVisibility.ancestors(7, in: cycle) == [7, 8])
    #expect(ViewerVisibility.subtree(7, in: cycle) == [7, 8])
}

@Test func disabledModalMetadataRemainsBackwardCompatible() throws {
    let state = WindowInfo(windowId: 1, kind: .dialog, isDisabled: true, isModal: true)
    #expect(try WindowInfo.decode(from: state.encode()) == state)
    let old = try WindowInfo.decode(from: WindowInfo(windowId: 2).encode())
    #expect(!old.isDisabled && !old.isModal)
}

@MainActor @Test func titlebarCloseOnlyHidesLocallyAndReleasesInput() async throws {
    _ = NSApplication.shared
    let wc = RemoteWindowController(info: WindowInfo(windowId: 1, kind: .normal,
        width: 640, height: 480, isResizable: true), owner: nil)
    defer { wc.forceCloseLocally() }
    var messages: [ControlMessage.Payload] = []
    wc.onSendControl = { messages.append($0) }
    wc.onDetachRequested = { _ in wc.hideLocally() }
    wc.remoteView.emitControl(.mouseEvent(MouseEvent(windowId: 1, kind: .buttonDown,
        normX: 0.2, normY: 0.3, button: .left)))
    wc.window?.performClose(nil)
    #expect(wc.isLocallyHidden)
    #expect(wc.window?.isVisible == false)
    #expect(!wc.remoteView.inputEnabled)
    #expect(messages.contains { if case .mouseEvent(let e) = $0 { return e.kind == .buttonUp }; return false })
    #expect(!messages.contains { if case .closeWindow = $0 { return true }; return false })
    #expect(!messages.contains { if case .unsubscribeWindow = $0 { return true }; return false })
    wc.applyUpdate(WindowUpdated(window: wc.info), owner: nil)
    wc.present(owner: nil)
    #expect(wc.window?.isVisible == false) // snapshots cannot resurrect a hidden viewer
    let render = wc.decoder.onDecodedFrame
    await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
        wc.decoder.onDecodedFrame = { image, width, height in
            render?(image, width, height)
            DispatchQueue.main.async { done.resume() }
        }
        let pixels = Data(repeating: 255, count: 16)
        wc.handleMediaFrame(MediaFrame(header: MediaFrameHeader(windowId: 1, timestampUs: 0,
            width: 2, height: 2, codec: .rawBgra, flags: [.keyframe], frameSeq: 1, payloadLen: 16), payload: pixels))
    }
    #expect(wc.window?.isVisible == false) // actual late decoding must not reopen it
    wc.restoreLocally(owner: nil)
    #expect(!wc.isLocallyHidden && wc.remoteView.inputEnabled)
    #expect(wc.window?.isVisible == true)
    messages.removeAll()
    let window = try #require(wc.window)
    let before = window.frame
    #expect(!wc.windowShouldZoom(window, toFrame: before.insetBy(dx: -50, dy: -50)))
    #expect(!messages.contains { if case .resizeWindow = $0 { return true }; return false })
    #expect(!wc.windowShouldZoom(window, toFrame: before))
    #expect(window.frame == before)
    wc.windowWillEnterFullScreen(Notification(name: NSWindow.willEnterFullScreenNotification, object: window))
    wc.windowWillStartLiveResize(Notification(name: NSWindow.willStartLiveResizeNotification, object: window))
    wc.windowDidEnterFullScreen(Notification(name: NSWindow.didEnterFullScreenNotification, object: window))
    wc.windowDidEndLiveResize(Notification(name: NSWindow.didEndLiveResizeNotification, object: window))
    #expect(!messages.contains { if case .resizeWindow = $0 { return true }; return false })
}

@MainActor @Test func ownedPanelsStayOutOfWindowMenuAndModalParentCanCloseLocally() throws {
    _ = NSApplication.shared
    let root = RemoteWindowController(info: WindowInfo(windowId: 1, kind: .normal,
        width: 640, height: 480, isDisabled: true), owner: nil)
    let panel = RemoteWindowController(info: WindowInfo(windowId: 2, ownerWindowId: 1,
        kind: .dialog, width: 320, height: 200, isModal: true), owner: root)
    let image = RemoteWindowController(info: WindowInfo(windowId: 3, ownerWindowId: 1,
        kind: .normal, width: 320, height: 200), owner: root)
    defer { panel.forceCloseLocally(); image.forceCloseLocally(); root.forceCloseLocally() }
    root.present(owner: nil)
    panel.present(owner: root)
    image.present(owner: root)
    #expect(panel.window is NSPanel)
    #expect(panel.window?.parent === root.window)
    #expect(panel.window?.canBecomeKey == true)
    #expect(panel.window?.canBecomeMain == false)
    #expect(panel.window?.isExcludedFromWindowsMenu == true)
    #expect(panel.window?.styleMask.contains(.miniaturizable) == false)
    #expect(image.window?.parent == nil)
    #expect(image.window?.isExcludedFromWindowsMenu == false)
    let panelWindow = try #require(panel.window)
    panelWindow.setFrameOrigin(NSPoint(x: panelWindow.frame.minX + 17, y: panelWindow.frame.minY + 11))
    panel.windowDidMove(Notification(name: NSWindow.didMoveNotification, object: panelWindow))
    let movedPanelFrame = panelWindow.frame
    #expect(!root.remoteView.inputEnabled)
    #expect(root.window?.standardWindowButton(.closeButton)?.isEnabled == true)
    root.onDetachRequested = { _ in root.hideLocally(); panel.hideLocally() }
    root.window?.performClose(nil)
    #expect(root.isLocallyHidden && panel.isLocallyHidden)
    #expect(image.window?.isVisible == true)
    root.restoreLocally(owner: nil)
    panel.restoreLocally(owner: root)
    #expect(!root.remoteView.inputEnabled) // hiding a modal did not dismiss it on Windows
    #expect(panel.window?.parent === root.window)
    #expect(panelWindow.frame == movedPanelFrame)
}

@MainActor @Test func ownedPanelsNeverInheritOrOverwriteMainWindowLayouts() throws {
    _ = NSApplication.shared
    let rootInfo = WindowInfo(windowId: 100, processName: "Synthetic.exe", kind: .normal,
        width: 1400, height: 1000, dpi: 144)
    let panelInfo = WindowInfo(windowId: 101, ownerWindowId: 100, processName: "Synthetic.exe",
        kind: .dialog, width: 400, height: 300, offsetX: 90, offsetY: 80, dpi: 144)
    let root = RemoteWindowController(info: rootInfo, owner: nil)
    let panel = RemoteWindowController(info: panelInfo, owner: root)
    defer { panel.forceCloseLocally(); root.forceCloseLocally() }
    #expect(AppState.layoutKey(for: rootInfo) == "proc:synthetic.exe")
    #expect(AppState.layoutKey(for: panelInfo) == nil)
    #expect(panel.snapshotLayout() == nil)
    // Previously the same process key applied this main-window frame to the panel.
    let panelWindow = try #require(panel.window)
    let before = panelWindow.frame
    let rootLayout = try #require(root.snapshotLayout())
    panel.applySavedLayout(rootLayout)
    #expect(panelWindow.frame == before)
    #expect(!panel.isPinnedOnTop && !panel.isAudioMuted)
    let video = root.videoRectInScreen
    #expect(abs(panelWindow.contentLayoutRect.width - video.width * 400 / 1400) <= 1)
    #expect(abs(panelWindow.contentLayoutRect.height - video.height * 300 / 1000) <= 1)

    // A panel may change size without changing its anchor; it must still use the
    // parent's scale, even when the client is in local-scale mode.
    var resized = panelInfo
    resized.width = 600; resized.height = 450
    panel.applyUpdate(WindowUpdated(window: resized), owner: root)
    #expect(abs(panelWindow.contentLayoutRect.width - video.width * 600 / 1400) <= 1)
    #expect(abs(panelWindow.contentLayoutRect.height - video.height * 450 / 1000) <= 1)

    // Related independent document/image viewers retain layout persistence.
    var independent = panelInfo
    independent.kind = .normal
    #expect(AppState.layoutKey(for: independent) == "proc:synthetic.exe")
    #expect(independent.usesIndependentLayout)
    #expect(!panelInfo.usesIndependentLayout)
}

@Test func occlusionIdentityOnlyAcceptsExactHostRejection() {
    #expect(ViewerVisibility.obscuringWindowID(from: "TARGET_OBSCURED blocker=123") == 123)
    #expect(ViewerVisibility.obscuringWindowID(from: "TARGET_OBSCURED blocker=0") == 0)
    for invalid in ["other error", "TARGET_OBSCURED blocker=-1", "TARGET_OBSCURED blocker=12 other", "TARGET_OBSCURED blocker=18446744073709551616"] {
        #expect(ViewerVisibility.obscuringWindowID(from: invalid) == nil)
    }
}
