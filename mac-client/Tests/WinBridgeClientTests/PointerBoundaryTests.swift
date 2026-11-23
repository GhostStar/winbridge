import AppKit
import Testing
@testable import WinBridgeCore

@MainActor private final class PointerFixture {
    let window: NSWindow
    let view: RemoteWindowView
    var messages: [MouseEvent] = []

    init(id: UInt64, source: NSSize = NSSize(width: 640, height: 480)) {
        _ = NSApplication.shared
        let bounds = NSRect(x: 0, y: 0, width: 640, height: 480)
        window = NSWindow(contentRect: bounds, styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.title = "Pointer boundary fixture"
        view = RemoteWindowView(windowId: id, frame: bounds)
        window.contentView = view
        view.updateDisplayGeometry(sourceSize: source, preserveAspect: true)
        view.onSendControl = { [weak self] in if case .mouseEvent(let event) = $0 { self?.messages.append(event) } }
        window.setFrameOrigin(NSPoint(x: id == 2 ? 800 : 100, y: 200))
        window.level = .floating
        window.orderFrontRegardless()
        window.displayIfNeeded()
        CATransaction.flush()
        RunLoop.current.run(until: Date().addingTimeInterval(0.03))
    }

    func event(_ type: NSEvent.EventType, _ point: NSPoint) throws -> NSEvent {
        try #require(NSEvent.mouseEvent(with: type, location: view.convert(point, to: nil), modifierFlags: [],
            timestamp: 0, windowNumber: window.windowNumber, context: nil, eventNumber: 1, clickCount: 1, pressure: 0))
    }
    func exitEvent() throws -> NSEvent {
        try #require(NSEvent.enterExitEvent(with: .mouseExited, location: .zero, modifierFlags: [], timestamp: 0,
            windowNumber: window.windowNumber, context: nil, eventNumber: 1, trackingNumber: 0, userData: nil))
    }
    func close() { view.releaseAllHeldKeys(); window.orderOut(nil); window.close() }
}

@MainActor @Test func twoViewersOnlyForwardEventsInsideTheirOwnVideo() throws {
    let a = PointerFixture(id: 1), b = PointerFixture(id: 2)
    defer { a.close(); b.close() }
    a.view.mouseMoved(with: try a.event(.mouseMoved, NSPoint(x: 100, y: 100)))
    #expect(a.messages.count == 1)
    a.view.mouseExited(with: try a.exitEvent())
    for point in [NSPoint(x: -1, y: 100), NSPoint(x: 641, y: 100), NSPoint(x: 100, y: -1), NSPoint(x: 100, y: 481)] {
        a.view.mouseMoved(with: try a.event(.mouseMoved, point))
    }
    #expect(a.messages.count == 1)
    let bEvent = try b.event(.mouseMoved, NSPoint(x: 100, y: 100))
    a.view.mouseMoved(with: bEvent) // stale event delivered to the old viewer
    b.view.mouseMoved(with: bEvent)
    #expect(a.messages.count == 1)
    #expect(b.messages.count == 1 && b.messages[0].windowId == 2)
    a.view.mouseDown(with: try b.event(.leftMouseDown, NSPoint(x: 100, y: 100)))
    #expect(a.messages.count == 1) // foreign clicks cannot acquire the old target
}

@MainActor @Test func exitingDuringDragReleasesOnceWithoutWarpOrRestart() throws {
    let a = PointerFixture(id: 1), b = PointerFixture(id: 2)
    defer { a.close(); b.close() }
    a.view.mouseDown(with: try a.event(.leftMouseDown, NSPoint(x: 120, y: 100)))
    a.view.mouseDragged(with: try a.event(.leftMouseDragged, NSPoint(x: 180, y: 130)))
    a.view.mouseExited(with: try a.exitEvent())
    #expect(a.messages.count == 3)
    let release = try #require(a.messages.last)
    #expect(release.kind == .buttonUp && release.button == .left && release.isRelative)
    #expect(release.deltaX == 0 && release.deltaY == 0 && release.modifiers == 0)
    a.view.mouseDragged(with: try a.event(.leftMouseDragged, NSPoint(x: 700, y: 100)))
    a.view.mouseDragged(with: try a.event(.leftMouseDragged, NSPoint(x: 200, y: 100)))
    a.view.mouseUp(with: try a.event(.leftMouseUp, NSPoint(x: 200, y: 100)))
    #expect(a.messages.count == 3) // re-entering with button held cannot resurrect the drag
    b.view.mouseDown(with: try b.event(.leftMouseDown, NSPoint(x: 200, y: 100)))
    a.view.mouseExited(with: try a.exitEvent()) // late exit cannot release the other viewer's button
    b.view.mouseUp(with: try b.event(.leftMouseUp, NSPoint(x: 200, y: 100)))
    #expect(a.messages.count == 3)
    #expect(b.messages.map(\.kind) == [.buttonDown, .buttonUp])
}

@MainActor @Test func missingExitEventStillStopsDragAtTheBoundary() throws {
    let fixture = PointerFixture(id: 3)
    defer { fixture.close() }
    fixture.view.rightMouseDown(with: try fixture.event(.rightMouseDown, NSPoint(x: 100, y: 100)))
    fixture.view.rightMouseDragged(with: try fixture.event(.rightMouseDragged, NSPoint(x: -20, y: 100)))
    fixture.view.rightMouseUp(with: try fixture.event(.rightMouseUp, NSPoint(x: -20, y: 100)))
    #expect(fixture.messages.count == 2)
    #expect(fixture.messages.last?.kind == .buttonUp)
    #expect(fixture.messages.last?.isRelative == true)
}

@MainActor @Test func letterboxMarginsAndHiddenViewsDoNotForwardPointer() throws {
    let fixture = PointerFixture(id: 4, source: NSSize(width: 1600, height: 900))
    defer { fixture.close() }
    #expect(fixture.view.currentVideoRect.minY > 0)
    fixture.view.mouseMoved(with: try fixture.event(.mouseMoved, NSPoint(x: 100, y: 5)))
    fixture.view.mouseDown(with: try fixture.event(.leftMouseDown, NSPoint(x: 100, y: 5)))
    fixture.view.mouseUp(with: try fixture.event(.leftMouseUp, NSPoint(x: 100, y: 5)))
    #expect(fixture.messages.isEmpty)
    fixture.view.mouseMoved(with: try fixture.event(.mouseMoved, NSPoint(x: 100, y: 100)))
    #expect(fixture.messages.count == 1)
    fixture.view.isHidden = true
    fixture.view.mouseMoved(with: try fixture.event(.mouseMoved, NSPoint(x: 100, y: 100)))
    #expect(fixture.messages.count == 1)
}

@MainActor @Test func invalidNormalizedPointerCannotReachHostAndFocusReleaseDoesNotMove() {
    let fixture = PointerFixture(id: 5)
    defer { fixture.close() }
    for coordinate: Float in [-0.1, 1.1, .nan, .infinity] {
        fixture.view.emitControl(.mouseEvent(MouseEvent(windowId: 5, kind: .move, normX: coordinate, normY: 0.5)))
    }
    #expect(fixture.messages.isEmpty)
    fixture.view.emitControl(.mouseEvent(MouseEvent(windowId: 5, kind: .buttonDown, normX: 0.8, normY: 0.7, button: .left)))
    fixture.view.releaseAllHeldKeys()
    fixture.view.releaseAllHeldKeys()
    #expect(fixture.messages.count == 2)
    #expect(fixture.messages.last?.isRelative == true)
}

@MainActor @Test func coveredViewerCannotMovePointerEvenWithItsOwnStaleEvent() throws {
    let a = PointerFixture(id: 1), b = PointerFixture(id: 2)
    defer { a.close(); b.close() }
    a.view.mouseMoved(with: try a.event(.mouseMoved, NSPoint(x: 100, y: 100)))
    #expect(a.messages.count == 1)
    b.window.setFrame(a.window.frame, display: true)
    b.window.orderFrontRegardless()
    b.window.displayIfNeeded()
    CATransaction.flush()
    RunLoop.current.run(until: Date().addingTimeInterval(0.03))
    a.view.mouseMoved(with: try a.event(.mouseMoved, NSPoint(x: 100, y: 100)))
    #expect(a.messages.count == 1)
    b.view.mouseMoved(with: try b.event(.mouseMoved, NSPoint(x: 100, y: 100)))
    #expect(b.messages.count == 1)
}
