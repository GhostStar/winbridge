import AppKit
import Testing
@testable import WinBridgeCore

@Test func fineTrackpadGesturesReachLegacyControlsInBothDirections() {
    // Model a control that truncates each incoming event to complete notches.
    // Old WinBridge: 2.5pt * 3 = 7.5 units per event, all discarded by that control.
    for direction in [1.0, -1.0] {
        var mapper = ScrollDeltaMapper()
        var notches = 0
        for i in 0..<40 {
            let output = mapper.map(x: 0, y: 2.5 * direction, precise: true,
                compatible: true, began: i == 0)
            notches += Int(output.y) / 120
            #expect(output.x == 0)
        }
        #expect(notches == Int(direction) * 10)
    }
}

@Test func reversingDirectionAndNewGesturesDoNotInheritOldRemainders() {
    var mapper = ScrollDeltaMapper()
    #expect(mapper.map(x: 0, y: 9, precise: true, compatible: true).y == 0)
    #expect(mapper.map(x: 0, y: -10, precise: true, compatible: true).y == -120)
    #expect(mapper.map(x: 0, y: 9, precise: true, compatible: true).y == 0)
    #expect(mapper.map(x: 0, y: 1, precise: true, compatible: true, began: true).y == 0)
    mapper.reset() // focus loss / local close / exit
    #expect(mapper.map(x: 0, y: 9, precise: true, compatible: true).y == 0)
}

@Test func smoothModeMouseNotchesAndHorizontalSignsStayAvailable() {
    var mapper = ScrollDeltaMapper()
    let smooth = mapper.map(x: 2.5, y: -2.5, precise: true, compatible: false)
    #expect(smooth.x == -30 && smooth.y == -30)
    let mouse = mapper.map(x: -1, y: 1, precise: false, compatible: true)
    #expect(mouse.x == 120 && mouse.y == 120)
    let horizontal = mapper.map(x: 10, y: 0, precise: true, compatible: true)
    #expect(horizontal.x == -120 && horizontal.y == 0)
    #expect(mapper.map(x: 0, y: .infinity, precise: true, compatible: true).y == 0)
    #expect(mapper.map(x: 0, y: 100000, precise: true, compatible: true).y == 1200)
}

@Test func trackpadRemaindersAreIsolatedPerWindowAndMode() {
    var first = ScrollDeltaMapper(), second = ScrollDeltaMapper()
    #expect(first.map(x: 0, y: 9, precise: true, compatible: true).y == 0)
    #expect(second.map(x: 0, y: 1, precise: true, compatible: true).y == 0)
    #expect(first.map(x: 0, y: 1, precise: true, compatible: true).y == 120)
    #expect(first.map(x: 0, y: 9, precise: true, compatible: true).y == 0)
    #expect(first.map(x: 0, y: 1, precise: true, compatible: false).y == 12)
    #expect(first.map(x: 0, y: 1, precise: true, compatible: true).y == 0)
}
