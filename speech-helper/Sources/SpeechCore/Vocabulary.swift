// Vocabulary boosting for the final transcript: FluidAudio's CTC keyword
// spotting plus constrained rescoring ("Approach 2" in FluidAudio's
// Documentation/ASR/CustomVocabulary.md). Parakeet TDT 0.6B v2 has no CTC
// head, so a separate Parakeet CTC 110M encoder scores the meeting's terms
// (attendee names, teams, project titles) against the audio, and a term
// replaces the words it beats on acoustic evidence.
//
// FluidAudio's own `VocabularyBoostingSession` reads its tokenizer from
// FluidAudio's cache folder in Application Support, never from the app's
// models directory, so this builds the same pipeline from its public parts
// (`CtcModels.loadDirect`, `CtcTokenizer`, `CtcKeywordSpotter`,
// `VocabularyRescorer`) on `ModelLayout.ctc`.

import FluidAudio
import Foundation

/// A replacement FluidAudio decided on: `words[range]` become `term`.
public struct WordReplacement: Equatable, Sendable {
    public let range: Range<Int>
    public let term: String

    public init(range: Range<Int>, term: String) {
        self.range = range
        self.term = term
    }
}

public final class VocabularyBooster {
    public let directory: URL

    private var models: CtcModels?
    private var session: Session?

    // A meeting's vocabulary is short names and plain-English team and
    // project words ("Priya", "Growth"), the case FluidAudio's defaults
    // over-fire on: with them, "price" became "Priya" and "grow the" became
    // "Growth" in a test clip. In a transcript, a name nobody said is worse
    // than a misspelt one, so this is stricter than FluidAudio's defaults:

    /// The short-term boost taper and spotter-rescue floors FluidAudio
    /// recommends for short, colliding vocabularies (#702).
    static let rescorerConfig = VocabularyRescorer.Config(
        shortTermCbwTaperPivot: 5,
        spotterRescueMinSimilarity: 0.30,
        spotterRescueMultiWordMinSimilarity: 0.50)

    /// The least string similarity between the heard words and a term
    /// (FluidAudio's default is 0.50 to 0.60 by vocabulary size; the larger
    /// wins). 0.62 rejects "price" for "Priya" (0.6) and keeps "Siobhan Wynne"
    /// for "Siobhan Nguyen".
    static let minSimilarity: Float = 0.62

    /// The spotter, rescorer and thresholds for one list of terms. A meeting's
    /// final pass sends the same list with every segment, so it is kept.
    struct Session {
        let terms: [String]
        let vocabulary: CustomVocabularyContext
        let spotter: CtcKeywordSpotter
        let rescorer: VocabularyRescorer
        let minSimilarity: Float
        let cbw: Float
    }

    public init(directory: URL) {
        self.directory = directory
    }

    private func loadModels() async throws -> CtcModels {
        if let models { return models }
        let started = Date()
        try ModelLayout.require(ModelLayout.ctcFiles, in: directory)
        let loaded = try await CtcModels.loadDirect(from: directory, variant: .ctc110m)
        logLine("loaded Parakeet CTC 110M (vocabulary boosting) in \(Int(millis(since: started))) ms")
        models = loaded
        return loaded
    }

    private func session(for terms: [String]) async throws -> Session? {
        if let session, session.terms == terms { return session }
        let models = try await loadModels()
        let tokenizer = try await CtcTokenizer.load(from: directory)
        let tokenized = terms.compactMap { text -> CustomVocabularyTerm? in
            let ids = tokenizer.encode(text)
            return ids.isEmpty ? nil : CustomVocabularyTerm(text: text, ctcTokenIds: ids)
        }
        guard !tokenized.isEmpty else { return nil }
        let vocabulary = CustomVocabularyContext(terms: tokenized, minSimilarity: Self.minSimilarity)
        let spotter = CtcKeywordSpotter(models: models, blankId: models.vocabulary.count)
        let rescorer = try await VocabularyRescorer.create(
            spotter: spotter, vocabulary: vocabulary, config: Self.rescorerConfig, ctcModelDirectory: directory)
        let sizeConfig = ContextBiasingConstants.rescorerConfig(forVocabSize: tokenized.count)
        let made = Session(
            terms: terms, vocabulary: vocabulary, spotter: spotter, rescorer: rescorer,
            minSimilarity: max(sizeConfig.minSimilarity, vocabulary.minSimilarity), cbw: sizeConfig.cbw)
        session = made
        return made
    }

    /// The replacements FluidAudio's rescorer applies to a Parakeet result
    /// (`text` and its `tokenTimings`, on the clock of `audio`), as ranges of
    /// `buildWordTimings(from: tokenTimings)`. Empty when no term wins.
    public func replacements(
        text: String, tokenTimings: [TokenTiming], audio: [Float], terms: [String]
    ) async throws -> [WordReplacement] {
        guard !tokenTimings.isEmpty, !audio.isEmpty, let session = try await session(for: terms) else { return [] }
        // A cheap text-only check first: no candidate, no CTC inference.
        guard
            session.rescorer.hasCTCRescoringCandidates(
                transcript: text, tokenTimings: tokenTimings, minSimilarity: session.minSimilarity)
        else { return [] }
        let spot = try await session.spotter.spotKeywordsWithLogProbs(
            audioSamples: audio, customVocabulary: session.vocabulary, minScore: nil)
        guard !spot.logProbs.isEmpty else { return [] }
        // The non-mutating evidence API: the same decisions as
        // `ctcTokenRescore` (the `applied` outcome), with the word ranges
        // they cover, so the words keep their timings.
        let evidence = session.rescorer.ctcTokenEvaluateCandidates(
            transcript: text,
            tokenTimings: tokenTimings,
            logProbs: spot.logProbs,
            frameDuration: spot.frameDuration,
            cbw: session.cbw,
            marginSeconds: 0.5,
            minSimilarity: session.minSimilarity)
        let applied = evidence.candidates.filter { $0.legacyOutcome == .applied }
        guard !applied.isEmpty else { return [] }
        let words = buildWordTimings(from: tokenTimings).map(\.word)
        guard evidence.baseWords == words else {
            logLine("vocabulary boosting: the rescorer's words do not match the transcript's; keeping it as is")
            return []
        }
        return applied.map { WordReplacement(range: $0.wordRange, term: $0.canonicalTerm) }
    }

    // MARK: - Applying

    static let leadingPunctuation = Set("\"“‘([{¿¡")
    static let trailingPunctuation = Set(".,!?;:)]}\"”’…")

    /// `words` with each replacement's words merged into one word holding the
    /// term, from the first word's start to the last word's end. Punctuation
    /// around the replaced words stays (`Dev,` becomes `Devansh,`), and a
    /// capitalised word keeps its capital. Out-of-range or overlapping
    /// replacements are ignored.
    public static func apply(_ replacements: [WordReplacement], to words: [Word]) -> (
        words: [Word], replaced: [Replacement]
    ) {
        var out: [Word] = []
        var replaced: [Replacement] = []
        var next = 0
        for replacement in replacements.sorted(by: { $0.range.lowerBound < $1.range.lowerBound }) {
            let range = replacement.range
            guard !range.isEmpty, range.lowerBound >= next, range.upperBound <= words.count,
                !replacement.term.isEmpty
            else { continue }
            out.append(contentsOf: words[next..<range.lowerBound])
            let first = words[range.lowerBound]
            let last = words[range.upperBound - 1]
            let leading = String(first.text.prefix { leadingPunctuation.contains($0) })
            let trailing = String(last.text.reversed().prefix { trailingPunctuation.contains($0) }.reversed())
            var term = replacement.term
            let firstLetter = first.text.first { $0.isLetter }
            if firstLetter?.isUppercase == true, let initial = term.first, initial.isLowercase {
                term = initial.uppercased() + term.dropFirst()
            }
            let text = leading + term + trailing
            let original = words[range].map(\.text).joined(separator: " ")
            out.append(Word(text: text, start: first.start, end: last.end))
            if text != original {
                replaced.append(Replacement(from: original, to: text))
            }
            next = range.upperBound
        }
        out.append(contentsOf: words[next...])
        return (out, replaced)
    }
}
