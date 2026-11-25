import Accelerate
import Foundation

/// Streaming conversion to 16 kHz. 48 kHz (the capture pipeline's rate) is
/// decimated by 3 through a windowed-sinc low-pass; 16 kHz passes through;
/// any other rate is linearly interpolated (not used by the app).
public final class Resampler {
    public let inputRate: Int
    public static let outputRate = 16_000

    private var pending: [Float] = []
    private let filter: [Float]
    private let factor: Int
    /// Linear interpolation read position for other rates.
    private var position: Double = 0

    public init(inputRate: Int) {
        self.inputRate = max(1, inputRate)
        if inputRate % Self.outputRate == 0 {
            factor = inputRate / Self.outputRate
        } else {
            factor = 0
        }
        if factor > 1 {
            filter = Self.lowpass(taps: 24 * factor + 1, cutoff: 0.45 / Double(factor))
            // Prime with zeros so the first output sample lines up with the
            // first input sample (the filter's group delay).
            pending = [Float](repeating: 0, count: filter.count / 2)
        } else {
            filter = []
        }
    }

    /// Hann-windowed sinc, unity gain at DC. `cutoff` is a fraction of the
    /// input rate.
    static func lowpass(taps: Int, cutoff: Double) -> [Float] {
        let mid = Double(taps - 1) / 2
        var h = (0..<taps).map { i -> Double in
            let x = Double(i) - mid
            let sinc = x == 0 ? 2 * cutoff : sin(2 * Double.pi * cutoff * x) / (Double.pi * x)
            let window = 0.5 - 0.5 * cos(2 * Double.pi * Double(i) / Double(taps - 1))
            return sinc * window
        }
        let sum = h.reduce(0, +)
        h = h.map { $0 / sum }
        return h.map { Float($0) }
    }

    public func process(_ input: [Float]) -> [Float] {
        if factor == 1 { return input }
        if factor > 1 {
            pending.append(contentsOf: input)
            let taps = filter.count
            guard pending.count >= taps else { return [] }
            let outCount = (pending.count - taps) / factor + 1
            var out = [Float](repeating: 0, count: outCount)
            pending.withUnsafeBufferPointer { src in
                vDSP_desamp(src.baseAddress!, vDSP_Stride(factor), filter, &out,
                            vDSP_Length(outCount), vDSP_Length(taps))
            }
            pending.removeFirst(outCount * factor)
            return out
        }
        // Linear interpolation over `pending`; `position` indexes into it.
        pending.append(contentsOf: input)
        let step = Double(inputRate) / Double(Self.outputRate)
        var out: [Float] = []
        while position + 1 < Double(pending.count) {
            let i = Int(position)
            let frac = Float(position - Double(i))
            out.append(pending[i] + (pending[i + 1] - pending[i]) * frac)
            position += step
        }
        let consumed = min(Int(position), pending.count)
        pending.removeFirst(consumed)
        position -= Double(consumed)
        return out
    }
}
