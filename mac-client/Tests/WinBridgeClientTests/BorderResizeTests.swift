import AppKit
import Testing
@testable import WinBridgeCore

@Test func hostResizeUsesFrozenDisplayScaleInsteadOfDpiOrRetina() {
    let gesture = BorderResizeGesture(option: true, hostAllowed: true,
        source: NSSize(width: 1200, height: 800), displayed: NSSize(width: 400, height: 800.0 / 3),
        aspect: NSSize(width: 1200, height: 800))
    let (w, h) = gesture.hostSize(for: NSSize(width: 500, height: 300))
    #expect(w == 1500 && h == 900)
    #expect(gesture.changesHost)
    // Neither a 2x Retina screen nor Windows DPI is an input to this mapping.
    #expect(gesture.hostSize(for: NSSize(width: 400, height: 800.0 / 3)).0 == 1200)
    #expect(gesture.hostSize(for: NSSize(width: 1_000_000, height: 0)).0 == 16_384)
    #expect(gesture.hostSize(for: NSSize(width: 1_000_000, height: 0)).1 == 16)
}

@MainActor @Test func ordinaryBorderDragScalesLocallyAndKeepsAspect() throws {
    _ = NSApplication.shared
    let wc = controller()
    defer { wc.forceCloseLocally() }
    let window = try #require(wc.window)
    var resizeMessages: [ResizeWindow] = []
    wc.onSendControl = { if case .resizeWindow(let r) = $0 { resizeMessages.append(r) } }
    wc.beginBorderResize(option: false, pointerDown: true)
    let proposed = window.frameRect(forContentRect: NSRect(x: 0, y: 0, width: 800, height: 480)).size
    let fitted = wc.windowWillResize(window, to: proposed)
    window.setFrame(NSRect(origin: window.frame.origin, size: fitted), display: false)
    wc.windowDidResize(note(NSWindow.didResizeNotification, window))
    wc.windowDidEndLiveResize(note(NSWindow.didEndLiveResizeNotification, window))
    #expect(resizeMessages.isEmpty)
    #expect(abs(window.contentLayoutRect.width / window.contentLayoutRect.height - 4.0 / 3) < 0.001)
    #expect(window.contentLayoutRect.width == 800)
    #expect(window.contentLayoutRect.height == 600)
}

@MainActor @Test func optionGestureIsLatchedAndNextGestureDefaultsToLocal() throws {
    _ = NSApplication.shared
    let wc = controller(dpi: 192)
    defer { wc.forceCloseLocally() }
    let window = try #require(wc.window)
    var sent: [ResizeWindow] = []
    wc.onSendControl = { if case .resizeWindow(let r) = $0 { sent.append(r) } }
    // First resize locally to a non-default zoom (source=640x480, displayed=480x360).
    window.setContentSize(NSSize(width: 480, height: 360))
    window.contentView?.layoutSubtreeIfNeeded()
    let displayed = wc.remoteView.currentVideoRect.size
    wc.beginBorderResize(option: true, pointerDown: true)
    let target = NSSize(width: 600, height: 500)
    let frameSize = window.frameRect(forContentRect: NSRect(origin: .zero, size: target)).size
    #expect(wc.windowWillResize(window, to: frameSize) == frameSize)
    window.setContentSize(target)
    wc.windowDidResize(note(NSWindow.didResizeNotification, window))
    // No Option is held in this test process; the gesture still retains its start mode.
    wc.windowDidEndLiveResize(note(NSWindow.didEndLiveResizeNotification, window))
    let resize = try #require(sent.last)
    #expect(resize.width == UInt32((600 * 640 / displayed.width).rounded()))
    #expect(resize.height == UInt32((500 * 480 / displayed.height).rounded()))
    #expect(window.title.contains(ResizeInteractionText.active) == false)
    wc.beginBorderResize(option: false, pointerDown: true)
    window.setContentSize(NSSize(width: 620, height: 510))
    wc.windowDidEndLiveResize(note(NSWindow.didEndLiveResizeNotification, window))
    #expect(sent.count == 1)
}

@MainActor @Test func panelGeometryCannotOverrideLiveOrUnacknowledgedHostResize() throws {
    _ = NSApplication.shared
    let root = controller()
    let childInfo = WindowInfo(windowId: 2, ownerWindowId: 1, kind: .dialog,
        width: 320, height: 240, offsetX: 30, offsetY: 40, dpi: 96, isResizable: true)
    let child = RemoteWindowController(info: childInfo, owner: root)
    defer { child.forceCloseLocally(); root.forceCloseLocally() }
    let window = try #require(child.window)
    var sent: [ResizeWindow] = []
    child.onSendControl = { if case .resizeWindow(let r) = $0 { sent.append(r) } }
    child.beginBorderResize(option: true, pointerDown: true)
    window.setContentSize(NSSize(width: 500, height: 360))
    let draggingFrame = window.frame
    var stale = childInfo
    stale.width = 300; stale.height = 200; stale.offsetX = 100
    child.applyUpdate(WindowUpdated(window: stale, ackResizeSeq: 0), owner: root)
    child.repositionRelative(to: root)
    #expect(window.frame == draggingFrame)
    child.windowDidEndLiveResize(note(NSWindow.didEndLiveResizeNotification, window))
    let sentResize = try #require(sent.last)
    child.applyUpdate(WindowUpdated(window: stale, ackResizeSeq: sentResize.resizeSeq - 1), owner: root)
    child.repositionRelative(to: root)
    #expect(window.frame == draggingFrame)
    var accepted = childInfo
    accepted.width = sentResize.width - 20 // app enforces its own size constraint
    accepted.height = sentResize.height
    child.applyUpdate(WindowUpdated(window: accepted, ackResizeSeq: sentResize.resizeSeq), owner: root)
    #expect(abs(window.contentLayoutRect.width - CGFloat(accepted.width)) < 1)
    #expect(sent.count == 1) // geometry reconciliation must never echo a resize request
}

@MainActor @Test func unavailableTargetsAndLocalChromeNeverResizeHost() throws {
    _ = NSApplication.shared
    for info in [
        WindowInfo(windowId: 1, kind: .normal, width: 640, height: 480, isResizable: false),
        WindowInfo(windowId: 2, kind: .dialog, width: 640, height: 480, isResizable: true, isDisabled: true),
        WindowInfo(windowId: SyntheticWindow.primaryDesktop, kind: .normal, width: 640, height: 480, isResizable: true)
    ] {
        let wc = RemoteWindowController(info: info, owner: nil)
        defer { wc.forceCloseLocally() }
        let window = try #require(wc.window)
        var sent: [ResizeWindow] = []
        wc.onSendControl = { if case .resizeWindow(let r) = $0 { sent.append(r) } }
        wc.beginBorderResize(option: true, pointerDown: true)
        window.setContentSize(NSSize(width: 700, height: 500))
        wc.windowDidEndLiveResize(note(NSWindow.didEndLiveResizeNotification, window))
        #expect(sent.isEmpty)
    }
    let wc = controller()
    defer { wc.forceCloseLocally() }
    let window = try #require(wc.window)
    var sent: [ResizeWindow] = []
    wc.onSendControl = { if case .resizeWindow(let r) = $0 { sent.append(r) } }
    wc.beginBorderResize(option: true, pointerDown: false)
    window.setContentSize(NSSize(width: 700, height: 500))
    wc.windowDidEndLiveResize(note(NSWindow.didEndLiveResizeNotification, window))
    wc.windowWillEnterFullScreen(note(NSWindow.willEnterFullScreenNotification, window))
    wc.beginBorderResize(option: true, pointerDown: true)
    wc.windowDidResize(note(NSWindow.didResizeNotification, window))
    wc.windowDidEndLiveResize(note(NSWindow.didEndLiveResizeNotification, window))
    #expect(sent.isEmpty)
}

@MainActor @Test func hideOrCapabilityLossCancelsHostResize() throws {
    _ = NSApplication.shared
    let wc = controller()
    defer { wc.forceCloseLocally() }
    let window = try #require(wc.window)
    var sent: [ResizeWindow] = []
    wc.onSendControl = { if case .resizeWindow(let r) = $0 { sent.append(r) } }
    wc.beginBorderResize(option: true, pointerDown: true)
    window.setContentSize(NSSize(width: 700, height: 500))
    wc.hideLocally()
    wc.windowDidEndLiveResize(note(NSWindow.didEndLiveResizeNotification, window))
    #expect(sent.isEmpty)
    wc.restoreLocally(owner: nil)
    wc.beginBorderResize(option: true, pointerDown: true)
    var disabled = wc.info; disabled.isDisabled = true
    wc.applyUpdate(WindowUpdated(window: disabled), owner: nil)
    wc.windowDidEndLiveResize(note(NSWindow.didEndLiveResizeNotification, window))
    #expect(sent.isEmpty)
}

@MainActor private func controller(dpi: UInt32 = 96) -> RemoteWindowController {
    RemoteWindowController(info: WindowInfo(windowId: 1, kind: .normal, width: 640, height: 480,
                                           dpi: dpi, isResizable: true), owner: nil)
}
private func note(_ name: Notification.Name, _ window: NSWindow) -> Notification { Notification(name: name, object: window) }

@MainActor @Test func optionResizeSendsDuringDragAndFlushesFinalSize() throws {
    _ = NSApplication.shared
    let wc = controller()
    defer { wc.forceCloseLocally() }
    let window = try #require(wc.window)
    var sent: [ResizeWindow] = []
    var forwarded: [ControlMessage.Payload] = []
    wc.onSendControl = {
        forwarded.append($0)
        if case .resizeWindow(let r) = $0 { sent.append(r) }
    }
    wc.beginBorderResize(option: true, pointerDown: true)
    // A second notification / modifier change cannot switch this drag back to local.
    wc.beginBorderResize(option: false, pointerDown: true)
    window.setContentSize(NSSize(width: 700, height: 500))
    wc.windowDidResize(note(NSWindow.didResizeNotification, window))
    wc.remoteView.emitControl(.mouseEvent(MouseEvent(windowId: 1, kind: .move, normX: 0.5, normY: 0.5)))
    // Swift Testing does not run AppKit's tracking loop while a Task sleeps.
    // Pump the actual native resize mode to verify common-mode timer delivery.
    let trackingUntil = Date().addingTimeInterval(0.2)
    while Date() < trackingUntil { _ = RunLoop.current.run(mode: .eventTracking, before: trackingUntil) }
    #expect(sent.count == 1)
    #expect(!forwarded.contains { if case .mouseEvent = $0 { return true }; return false })
    window.setContentSize(NSSize(width: 720, height: 510))
    wc.windowDidResize(note(NSWindow.didResizeNotification, window))
    wc.windowDidEndLiveResize(note(NSWindow.didEndLiveResizeNotification, window))
    #expect(sent.count == 2)
    #expect(sent.last?.width == 720 && sent.last?.height == 510)
    RunLoop.current.run(until: Date().addingTimeInterval(0.15))
    #expect(sent.count == 2) // no delayed request after mouse-up
    #expect(!wc.remoteView.localBorderGesture)
}

@Test func localResizeAxisDoesNotOscillateWithAppKitFeedback() {
    var horizontal = BorderResizeGesture(option: false, hostAllowed: true,
        source: NSSize(width: 640, height: 480), displayed: NSSize(width: 640, height: 480),
        aspect: NSSize(width: 640, height: 480))
    // AppKit's proposal on the non-dragged axis can lag behind our aspect correction.
    // Using the last corrected frame would repeatedly switch the driving axis.
    for w in stride(from: CGFloat(650), through: 900, by: 10) {
        let fitted = horizontal.proportional(NSSize(width: w, height: 480))
        #expect(fitted == NSSize(width: w, height: w * 0.75))
    }
    var vertical = BorderResizeGesture(option: false, hostAllowed: true,
        source: NSSize(width: 640, height: 480), displayed: NSSize(width: 640, height: 480),
        aspect: NSSize(width: 640, height: 480))
    for h in stride(from: CGFloat(490), through: 700, by: 10) {
        let fitted = vertical.proportional(NSSize(width: 640, height: h))
        #expect(abs(fitted.width - h * 4 / 3) < 0.001)
        #expect(abs(fitted.height - h) < 0.001)
    }
}

@MainActor @Test func optionIsCapturedBeforeFirstNativeSizeQueryForBothWindowTypes() throws {
    _ = NSApplication.shared
    for kind: WindowKind in [.normal, .dialog] {
        let wc = RemoteWindowController(info: WindowInfo(windowId: 9, kind: kind,
            width: 640, height: 480, isResizable: true), owner: nil)
        defer { wc.forceCloseLocally() }
        let window = try #require(wc.window)
        let source = try #require(window as? NativeBorderEventSource)
        let down = try #require(NSEvent.mouseEvent(with: .leftMouseDown,
            location: .zero, modifierFlags: [.option], timestamp: 0,
            windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        var sent: [ResizeWindow] = []
        wc.onSendControl = { if case .resizeWindow(let size) = $0 { sent.append(size) } }
        let wanted = NSSize(width: 800, height: 550) // deliberately not 4:3
        let frame = window.frameRect(forContentRect: NSRect(origin: .zero, size: wanted)).size
        source.borderEvent.dispatch(down) {
            #expect(wc.windowWillResize(window, to: frame) == frame)
            wc.windowWillStartLiveResize(note(NSWindow.willStartLiveResizeNotification, window))
            window.setContentSize(wanted)
        }
        #expect(source.borderEvent.modifiers == nil)
        wc.windowDidEndLiveResize(note(NSWindow.didEndLiveResizeNotification, window))
        #expect(sent.last?.width == 800 && sent.last?.height == 550)
    }
}

@MainActor @Test func programmaticResizeDoesNotEnterAspectFeedback() throws {
    _ = NSApplication.shared
    let wc = controller()
    defer { wc.forceCloseLocally() }
    let window = try #require(wc.window)
    let frame = NSSize(width: 715, height: 543)
    #expect(wc.windowWillResize(window, to: frame) == frame)
}
