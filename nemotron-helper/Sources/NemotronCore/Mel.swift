// Log-mel features for the Nemotron streaming encoder, as NeMo's
// AudioToMelSpectrogramPreprocessor computes them for this model:
// pre-emphasis 0.97, centred STFT (symmetric Hann 400, 512-point FFT, 10 ms
// hop), power spectrum, Slaney mel filterbank, log(x + 2^-24), and no
// normalization. Frames come from the continuous signal (not per chunk), so
// chunk boundaries see the same features as an offline pass.
//
// The FFT and filterbank code is adapted from speech-swift's
// StreamingMelPreprocessor (https://github.com/soniqo/speech-swift,
// Apache-2.0).

import Accelerate
import Foundation

public final class MelExtractor {
    struct Params {
        let numMelBins: Int
        let sampleRate: Int
        let hopLength: Int
        let winLength: Int
        let preEmphasis: Float
    }

    let config: Params
    private let fftSize = 512
    private let log2FFT: vDSP_Length = 9
    private let bins = 257
    private let logGuard: Float = 5.960464477539063e-08
    private let fftSetup: FFTSetup
    private let window: [Float]
    private let filterbank: [Float]  // [mels x bins]

    /// `periodicWindow`: periodic Hann (librosa's default) or symmetric
    /// (`torch.hann_window(periodic=False)`, what NeMo uses).
    public init(numMelBins: Int, sampleRate: Int, hop: Int, winLength: Int,
                preEmphasis: Float, periodicWindow: Bool) {
        config = Params(numMelBins: numMelBins, sampleRate: sampleRate, hopLength: hop,
                        winLength: winLength, preEmphasis: preEmphasis)
        let denominator = Float(periodicWindow ? winLength : winLength - 1)
        window = (0..<winLength).map {
            0.5 * (1 - cos(2 * Float.pi * Float($0) / denominator))
        }
        fftSetup = vDSP_create_fftsetup(9, FFTRadix(kFFTRadix2))!
        filterbank = Self.melFilterbank(mels: numMelBins, bins: 257,
                                        sampleRate: sampleRate, fftSize: 512)
    }

    /// Log-mel frames `first ..< first + count` of a continuous signal, as
    /// NeMo computes them over a whole recording: frame `k` is centred on
    /// sample `k * hop`, and samples outside the signal are zero.
    /// `emphasized` holds the pre-emphasized signal from absolute sample
    /// `base` on. Returns `[mels x count]` row-major.
    public func frames(_ emphasized: [Float], base: Int, first: Int, count: Int) -> [Float] {
        let half = fftSize / 2
        let winOffset = (fftSize - config.winLength) / 2
        var power = [Float](repeating: 0, count: count * bins)
        var real = [Float](repeating: 0, count: half)
        var imag = [Float](repeating: 0, count: half)
        var frame = [Float](repeating: 0, count: fftSize)
        for f in 0..<count {
            let start = (first + f) * config.hopLength - half  // absolute
            for i in 0..<fftSize { frame[i] = 0 }
            for i in winOffset..<(winOffset + config.winLength) {
                let index = start + i - base
                if index >= 0, index < emphasized.count {
                    frame[i] = emphasized[index] * window[i - winOffset]
                }
            }
            for i in 0..<half {
                real[i] = frame[2 * i]
                imag[i] = frame[2 * i + 1]
            }
            real.withUnsafeMutableBufferPointer { r in
                imag.withUnsafeMutableBufferPointer { im in
                    var split = DSPSplitComplex(realp: r.baseAddress!, imagp: im.baseAddress!)
                    vDSP_fft_zrip(fftSetup, &split, 1, log2FFT, FFTDirection(kFFTDirection_Forward))
                }
            }
            let b = f * bins
            power[b] = real[0] * real[0] * 0.25
            power[b + half] = imag[0] * imag[0] * 0.25
            for k in 1..<half {
                power[b + k] = (real[k] * real[k] + imag[k] * imag[k]) * 0.25
            }
        }
        var powerT = [Float](repeating: 0, count: bins * count)
        vDSP_mtrans(power, 1, &powerT, 1, vDSP_Length(bins), vDSP_Length(count))
        var mel = [Float](repeating: 0, count: config.numMelBins * count)
        vDSP_mmul(filterbank, 1, powerT, 1, &mel, 1,
                  vDSP_Length(config.numMelBins), vDSP_Length(count), vDSP_Length(bins))
        var guardValue = logGuard
        vDSP_vsadd(mel, 1, &guardValue, &mel, 1, vDSP_Length(mel.count))
        var n = Int32(mel.count)
        vvlogf(&mel, mel, &n)
        return mel
    }

    /// Slaney-style mel filterbank with Slaney normalization, `[mels x bins]`.
    static func melFilterbank(mels: Int, bins: Int, sampleRate: Int, fftSize: Int) -> [Float] {
        let minLogHz: Float = 1000, minLogMel: Float = 15
        let logStep: Float = 27 / log(6.4)
        func hzToMel(_ hz: Float) -> Float {
            hz < minLogHz ? 3 * hz / 200 : minLogMel + log(hz / minLogHz) * logStep
        }
        func melToHz(_ mel: Float) -> Float {
            mel < minLogMel ? 200 * mel / 3 : minLogHz * exp((mel - minLogMel) / logStep)
        }
        let melMax = hzToMel(Float(sampleRate) / 2)
        let points = (0..<(mels + 2)).map { melToHz(Float($0) * melMax / Float(mels + 1)) }
        var bank = [Float](repeating: 0, count: mels * bins)
        for m in 0..<mels {
            let lower = points[m], center = points[m + 1], upper = points[m + 2]
            let norm = 2 / (upper - lower)
            for b in 0..<bins {
                let freq = Float(b) * Float(sampleRate) / Float(fftSize)
                let down = (freq - lower) / (center - lower)
                let up = (upper - freq) / (upper - center)
                bank[m * bins + b] = max(0, min(down, up)) * norm
            }
        }
        return bank
    }
}
