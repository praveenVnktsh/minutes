import CoreML
import Foundation

/// Where the models run. Named after what the user picks in Settings.
public enum ComputeChoice: String, CaseIterable, Sendable {
    /// Core ML on the GPU (and CPU for what the GPU cannot run).
    case gpu
    /// Core ML on the Neural Engine (and CPU).
    case ane
    /// Core ML on the CPU only.
    case cpu
    /// Core ML decides (all units).
    case all

    public var units: MLComputeUnits {
        switch self {
        case .gpu: return .cpuAndGPU
        case .ane: return .cpuAndNeuralEngine
        case .cpu: return .cpuOnly
        case .all: return .all
        }
    }
}

public enum HelperError: Error, CustomStringConvertible {
    case missing(String)
    case badOutput(String)

    public var description: String {
        switch self {
        case .missing(let what): return "missing \(what)"
        case .badOutput(let what): return "unexpected model output: \(what)"
        }
    }
}

extension MLMultiArray {
    /// Element `i` of the array's storage as Float, whatever its scalar type.
    @inline(__always)
    func float(at i: Int) -> Float {
        switch dataType {
        case .float16:
            return Float(dataPointer.assumingMemoryBound(to: Float16.self)[i])
        case .float32:
            return dataPointer.assumingMemoryBound(to: Float.self)[i]
        case .double:
            return Float(dataPointer.assumingMemoryBound(to: Double.self)[i])
        default:
            return self[i].floatValue
        }
    }

    static func zeros(_ shape: [Int], _ type: MLMultiArrayDataType) throws -> MLMultiArray {
        let array = try MLMultiArray(shape: shape.map { NSNumber(value: $0) }, dataType: type)
        let bytes: Int
        switch type {
        case .float16: bytes = 2
        case .double: bytes = 8
        default: bytes = 4
        }
        memset(array.dataPointer, 0, array.count * bytes)
        return array
    }
}
