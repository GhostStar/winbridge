import Foundation

/// Per-view gesture state. Compatibility mode delivers complete Win32 wheel
/// detents for controls that discard small deltas instead of accumulating them.
struct ScrollDeltaMapper {
    private var remainder = [Double](repeating: 0, count: 2)
    private var previousMode: Bool?

    mutating func reset() {
        remainder = [0, 0]
        previousMode = nil
    }

    mutating func map(x: Double, y: Double, precise: Bool, compatible: Bool,
                      began: Bool = false) -> (x: Float, y: Float) {
        guard x.isFinite, y.isFinite else { reset(); return (0, 0) }
        if began || previousMode != compatible || !precise { reset() }
        previousMode = compatible
        // Match Kokoda's trackpad sensitivity. Mouse notches retain 120 units.
        let scale = precise ? 12.0 : 120.0
        let deltas = [-x, y].map { max(-1200, min(1200, $0 * scale)) }
        guard precise && compatible else { return (Float(deltas[0]), Float(deltas[1])) }
        var output = [Float](repeating: 0, count: 2)
        for axis in 0..<2 {
            let delta = deltas[axis]
            // A reversal must not first pay off the previous direction's tail.
            if remainder[axis] * delta < 0 { remainder[axis] = 0 }
            remainder[axis] += delta
            let steps = (remainder[axis] / 120).rounded(.towardZero)
            output[axis] = Float(steps * 120)
            remainder[axis] -= steps * 120
        }
        return (output[0], output[1])
    }
}
