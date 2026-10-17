import Foundation

/// SentencePiece pieces by id (`vocab.json`: `{"0": "<unk>", "1": "▁t", ...}`).
/// Pieces are reported with the word marker `▁` turned into a space, the form
/// sherpa-onnx reports tokens in, so the app joins them the same way.
public struct Vocabulary: Sendable {
    private let pieces: [String]

    public init(pieces: [String]) {
        self.pieces = pieces
    }

    public static func load(from url: URL) throws -> Vocabulary {
        let raw = try JSONDecoder().decode([String: String].self, from: Data(contentsOf: url))
        let count = (raw.keys.compactMap { Int($0) }.max() ?? -1) + 1
        var pieces = [String](repeating: "", count: count)
        for (key, value) in raw {
            if let id = Int(key), id >= 0, id < count { pieces[id] = value }
        }
        return Vocabulary(pieces: pieces)
    }

    public var count: Int { pieces.count }

    /// The token's text, or "" for special markers (`<unk>`, ...) and ids
    /// outside the vocabulary.
    public func text(_ id: Int) -> String {
        guard id >= 0, id < pieces.count else { return "" }
        let piece = pieces[id]
        if piece.count >= 3, piece.first == "<", piece.last == ">", !piece.contains(" ") {
            return ""
        }
        return piece.replacingOccurrences(of: "▁", with: " ")
    }
}
