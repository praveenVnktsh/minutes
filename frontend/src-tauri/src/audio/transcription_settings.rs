//! Live and final transcription settings, their migration, and the decision
//! whether a final pass runs.
//!
//! - The **live** model streams text during a recording: Nemotron Speech
//!   Streaming in the speech helper, on the Neural Engine or the GPU. There
//!   is no other live engine; without it a meeting is recorded without live
//!   text.
//! - The **final** model (Parakeet TDT v2 in the speech helper) re-transcribes
//!   the whole recording after the meeting and replaces the live transcript.
//!   It is fixed, so there is nothing to choose here, only whether and when
//!   it runs.
//!
//! Everything that decides something is a pure function here so it can be
//! unit tested; the async wrappers only gather inputs and apply results.

use log::{info, warn};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};
use tauri_plugin_store::StoreExt;

use crate::audio::speech::models::Availability;

const STORE_FILE: &str = "store.json";
pub const AUTO_FINAL_PASS_KEY: &str = "autoFinalTranscriptPass";
pub const LIVE_ENGINE_KEY: &str = "liveTranscriptionEngine";
/// When the final pass runs: [`FinalPassTiming`]. Replaces the on/off
/// [`AUTO_FINAL_PASS_KEY`], which is still read when this key is absent and
/// still written alongside it.
pub const FINAL_PASS_TIMING_KEY: &str = "finalTranscriptTiming";

/// `store.json` keys of settings that no longer exist: the live model choice
/// (Nemotron or the final engine on VAD chunks) and the flag of the one-time
/// Whisper final-model migration. [`migrate_settings`] deletes them.
const RETIRED_KEYS: [&str; 2] = ["liveTranscriptionModel", "finalTranscriptModelMigrated"];

/// Where the streaming live model runs (Settings → Advanced).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LiveEngineChoice {
    /// The measured best (see `live_streaming::engine::AUTO_COMPUTE`).
    Auto,
    /// Core ML on the GPU.
    Gpu,
    /// Core ML on the Neural Engine.
    NeuralEngine,
}

impl LiveEngineChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Gpu => "gpu",
            Self::NeuralEngine => "neural-engine",
        }
    }

    /// `"cpu"` (the retired CPU engine) reads as Auto.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" | "cpu" => Some(Self::Auto),
            "gpu" => Some(Self::Gpu),
            "neural-engine" => Some(Self::NeuralEngine),
            _ => None,
        }
    }

    /// Unset or unrecognised values mean Auto.
    pub fn from_stored(value: Option<&str>) -> Self {
        value.and_then(Self::parse).unwrap_or(Self::Auto)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveTranscriptionSettings {
    pub auto_final_pass: bool,
    pub live_engine: LiveEngineChoice,
}

impl LiveTranscriptionSettings {
    pub fn from_stored(auto_final_pass: Option<bool>, live_engine: Option<&str>) -> Self {
        Self {
            auto_final_pass: auto_final_pass.unwrap_or(true),
            live_engine: LiveEngineChoice::from_stored(live_engine),
        }
    }
}

/// When the automatic final pass ("Improve transcript after meetings") runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FinalPassTiming {
    /// Right after every meeting.
    Automatic,
    /// Right after a meeting on power; on battery or in Low Power Mode it
    /// waits until the Mac is plugged in and out of it (or the user asks). The default: the pass is the heaviest
    /// thing the app does after a meeting, and on battery it costs charge
    /// and fan noise for a transcript that is already readable.
    WhenPluggedIn,
    /// Never on its own; the user can still ask for it per meeting.
    Off,
}

impl FinalPassTiming {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::WhenPluggedIn => "when-plugged-in",
            Self::Off => "off",
        }
    }

    /// The stored choice; without one, the old on/off switch: off stays off,
    /// on (or never set) becomes the default.
    pub fn from_stored(timing: Option<&str>, legacy_auto: Option<bool>) -> Self {
        match timing {
            Some("automatic") => Self::Automatic,
            Some("when-plugged-in") => Self::WhenPluggedIn,
            Some("off") => Self::Off,
            _ if legacy_auto == Some(false) => Self::Off,
            _ => Self::WhenPluggedIn,
        }
    }
}

/// Why an automatic final pass did not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalPassSkip {
    /// The automatic pass is turned off and a live transcript exists.
    Disabled,
    /// The transcript was edited; replacing it would lose the edits.
    Edited,
    /// The transcript models (Parakeet and the voice detector) are not
    /// downloaded. Serialised under its old name, which the frontend reads.
    #[serde(rename = "no_engine")]
    NoModel,
}

impl FinalPassSkip {
    pub fn message(self) -> &'static str {
        match self {
            Self::Disabled => "The automatic final transcript is turned off.",
            Self::Edited => "The transcript has edits, so it was not replaced by a final pass.",
            Self::NoModel => "The transcript model is not downloaded. Download it in Settings → Transcription, then retranscribe the meeting.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalPassDecision {
    Run,
    /// Run it once the Mac is plugged in; the live transcript stands until then.
    WaitForPower,
    /// Run it once the transcript models finish downloading (right after an
    /// upgrade, or the first launch); the live transcript stands until then.
    WaitForModels,
    Skip(FinalPassSkip),
}

/// Whether the final pass runs.
/// - Edits always win: a transcript with revisions is never replaced.
/// - `force` (deferred mode, or the user asking for it) and an empty live
///   transcript (no live engine was available) run now, whatever the timing
///   and the battery, since otherwise the meeting would have no transcript
///   or the user would be kept waiting.
/// - Otherwise "When plugged in" waits while `saving_power` (on battery or
///   in Low Power Mode, see `quiet_work::saving_power_now`).
/// - Without the transcript models (`models`, see
///   `speech::models::availability`) it waits while they download, and
///   cannot run at all when they are not coming (macOS older than 15, or the
///   automatic download was cancelled).
pub fn decide_final_pass(
    timing: FinalPassTiming,
    saving_power: bool,
    force: bool,
    has_revisions: bool,
    has_transcripts: bool,
    models: Availability,
) -> FinalPassDecision {
    if has_revisions {
        return FinalPassDecision::Skip(FinalPassSkip::Edited);
    }
    let urgent = force || !has_transcripts;
    if timing == FinalPassTiming::Off && !urgent {
        return FinalPassDecision::Skip(FinalPassSkip::Disabled);
    }
    match models {
        Availability::Ready => {}
        Availability::Coming => return FinalPassDecision::WaitForModels,
        Availability::Unavailable => return FinalPassDecision::Skip(FinalPassSkip::NoModel),
    }
    if timing == FinalPassTiming::WhenPluggedIn && saving_power && !urgent {
        return FinalPassDecision::WaitForPower;
    }
    FinalPassDecision::Run
}

/// The value to store in place of a stored live engine, if it must change:
/// the removed CPU engine becomes Auto.
fn migrated_live_engine(stored: Option<&str>) -> Option<&'static str> {
    match stored {
        Some("cpu") => Some(LiveEngineChoice::Auto.as_str()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// App wrappers
// ---------------------------------------------------------------------------

pub fn load_settings<R: Runtime>(app: &AppHandle<R>) -> LiveTranscriptionSettings {
    match app.store(STORE_FILE) {
        Ok(store) => LiveTranscriptionSettings::from_stored(
            store
                .get(AUTO_FINAL_PASS_KEY)
                .as_ref()
                .and_then(|v| v.as_bool()),
            store.get(LIVE_ENGINE_KEY).as_ref().and_then(|v| v.as_str()),
        ),
        Err(error) => {
            warn!("Transcription settings store unavailable, using defaults: {error}");
            LiveTranscriptionSettings::from_stored(None, None)
        }
    }
}

/// When the final pass runs, from the store.
pub fn load_final_pass_timing<R: Runtime>(app: &AppHandle<R>) -> FinalPassTiming {
    match app.store(STORE_FILE) {
        Ok(store) => FinalPassTiming::from_stored(
            store
                .get(FINAL_PASS_TIMING_KEY)
                .as_ref()
                .and_then(|v| v.as_str()),
            store
                .get(AUTO_FINAL_PASS_KEY)
                .as_ref()
                .and_then(|v| v.as_bool()),
        ),
        Err(error) => {
            warn!("Transcription settings store unavailable, using defaults: {error}");
            FinalPassTiming::from_stored(None, None)
        }
    }
}

/// Save when the final pass runs. The old on/off key follows it, so anything
/// still reading that key agrees.
pub fn save_final_pass_timing<R: Runtime>(
    app: &AppHandle<R>,
    timing: FinalPassTiming,
) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    store.set(FINAL_PASS_TIMING_KEY, serde_json::json!(timing.as_str()));
    store.set(
        AUTO_FINAL_PASS_KEY,
        serde_json::json!(timing != FinalPassTiming::Off),
    );
    store.save().map_err(|e| e.to_string())
}

/// Clear out settings of the removed engines: delete the retired keys and
/// move a CPU live engine to Auto. The `transcript_settings` SQLite row
/// (provider and model of the old final engine) is left alone and no longer
/// read. Safe to call more than once.
pub async fn migrate_settings<R: Runtime>(app: &AppHandle<R>) {
    let Ok(store) = app.store(STORE_FILE) else {
        return;
    };
    let mut changed = false;
    for key in RETIRED_KEYS {
        if store.delete(key) {
            info!("Removed the retired transcription setting {key}");
            changed = true;
        }
    }
    let stored_engine = store.get(LIVE_ENGINE_KEY);
    if let Some(engine) = migrated_live_engine(stored_engine.as_ref().and_then(|v| v.as_str())) {
        info!("Live transcription engine moved from the removed CPU engine to {engine}");
        store.set(LIVE_ENGINE_KEY, serde_json::json!(engine));
        changed = true;
    }
    if changed {
        if let Err(error) = store.save() {
            warn!("Could not persist the transcription settings migration: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_settings_default_to_auto_engine_and_auto_final_pass() {
        let settings = LiveTranscriptionSettings::from_stored(None, None);
        assert!(settings.auto_final_pass);
        assert_eq!(settings.live_engine, LiveEngineChoice::Auto);
    }

    #[test]
    fn explicit_settings_are_respected() {
        let settings = LiveTranscriptionSettings::from_stored(Some(false), Some("gpu"));
        assert!(!settings.auto_final_pass);
        assert_eq!(settings.live_engine, LiveEngineChoice::Gpu);
    }

    #[test]
    fn settings_serialise_in_camel_case_without_a_live_model() {
        let json = serde_json::to_value(LiveTranscriptionSettings::from_stored(
            Some(true),
            Some("neural-engine"),
        ))
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "autoFinalPass": true, "liveEngine": "neural-engine" })
        );
    }

    #[test]
    fn live_engine_choices_round_trip_and_unknown_means_auto() {
        for choice in [
            LiveEngineChoice::Auto,
            LiveEngineChoice::Gpu,
            LiveEngineChoice::NeuralEngine,
        ] {
            assert_eq!(LiveEngineChoice::parse(choice.as_str()), Some(choice));
            let json = serde_json::to_string(&choice).unwrap();
            assert_eq!(json, format!("\"{}\"", choice.as_str()));
        }
        assert_eq!(LiveEngineChoice::from_stored(None), LiveEngineChoice::Auto);
        assert_eq!(
            LiveEngineChoice::from_stored(Some("tpu")),
            LiveEngineChoice::Auto
        );
        assert_eq!(LiveEngineChoice::parse("tpu"), None);
    }

    #[test]
    fn the_removed_cpu_engine_reads_as_auto_and_is_migrated() {
        assert_eq!(LiveEngineChoice::parse("cpu"), Some(LiveEngineChoice::Auto));
        assert_eq!(
            LiveEngineChoice::from_stored(Some("cpu")),
            LiveEngineChoice::Auto
        );
        assert_eq!(migrated_live_engine(Some("cpu")), Some("auto"));
        for kept in [None, Some("auto"), Some("gpu"), Some("neural-engine")] {
            assert_eq!(migrated_live_engine(kept), None, "{kept:?}");
        }
    }

    use Availability::{Coming, Ready, Unavailable};
    use FinalPassTiming::{Automatic, Off, WhenPluggedIn};

    #[test]
    fn final_pass_runs_by_default_after_a_live_transcript_on_power() {
        for timing in [Automatic, WhenPluggedIn] {
            assert_eq!(
                decide_final_pass(timing, false, false, false, true, Ready),
                FinalPassDecision::Run
            );
        }
    }

    #[test]
    fn on_battery_the_default_waits_for_power_and_automatic_does_not() {
        assert_eq!(
            decide_final_pass(WhenPluggedIn, true, false, false, true, Ready),
            FinalPassDecision::WaitForPower
        );
        assert_eq!(
            decide_final_pass(Automatic, true, false, false, true, Ready),
            FinalPassDecision::Run
        );
    }

    #[test]
    fn asking_for_it_or_having_no_live_transcript_never_waits() {
        // The user pressed "Improve now" (or deferred mode forced it).
        assert_eq!(
            decide_final_pass(WhenPluggedIn, true, true, false, true, Ready),
            FinalPassDecision::Run
        );
        // Without a live transcript the meeting would have no text at all.
        assert_eq!(
            decide_final_pass(WhenPluggedIn, true, false, false, false, Ready),
            FinalPassDecision::Run
        );
    }

    #[test]
    fn final_pass_never_replaces_an_edited_transcript_even_when_forced() {
        assert_eq!(
            decide_final_pass(Automatic, false, true, true, true, Ready),
            FinalPassDecision::Skip(FinalPassSkip::Edited)
        );
    }

    #[test]
    fn disabled_final_pass_still_runs_when_forced_or_without_a_live_transcript() {
        for on_battery in [false, true] {
            assert_eq!(
                decide_final_pass(Off, on_battery, false, false, true, Ready),
                FinalPassDecision::Skip(FinalPassSkip::Disabled)
            );
        }
        assert_eq!(
            decide_final_pass(Off, true, true, false, true, Ready),
            FinalPassDecision::Run
        );
        assert_eq!(
            decide_final_pass(Off, false, false, false, false, Ready),
            FinalPassDecision::Run
        );
    }

    #[test]
    fn final_pass_without_the_model_is_skipped_with_a_reason() {
        for on_battery in [false, true] {
            assert_eq!(
                decide_final_pass(WhenPluggedIn, on_battery, false, false, true, Unavailable),
                FinalPassDecision::Skip(FinalPassSkip::NoModel)
            );
        }
        // Forced or without a live transcript, it still cannot run.
        assert_eq!(
            decide_final_pass(Automatic, false, true, false, false, Unavailable),
            FinalPassDecision::Skip(FinalPassSkip::NoModel)
        );
        // Edits are still the reason when both apply.
        assert_eq!(
            decide_final_pass(Automatic, false, false, true, true, Unavailable),
            FinalPassDecision::Skip(FinalPassSkip::Edited)
        );
    }

    #[test]
    fn final_pass_waits_for_models_that_are_still_downloading() {
        for timing in [Automatic, WhenPluggedIn] {
            for on_battery in [false, true] {
                assert_eq!(
                    decide_final_pass(timing, on_battery, false, false, true, Coming),
                    FinalPassDecision::WaitForModels
                );
            }
        }
        // Forced or without a live transcript it waits too: it is the only
        // way the meeting gets a transcript.
        assert_eq!(
            decide_final_pass(Off, false, true, false, false, Coming),
            FinalPassDecision::WaitForModels
        );
        // Turned off or edited, there is nothing to wait for.
        assert_eq!(
            decide_final_pass(Off, false, false, false, true, Coming),
            FinalPassDecision::Skip(FinalPassSkip::Disabled)
        );
        assert_eq!(
            decide_final_pass(Automatic, false, false, true, true, Coming),
            FinalPassDecision::Skip(FinalPassSkip::Edited)
        );
    }

    #[test]
    fn skip_reasons_keep_their_wire_names() {
        assert_eq!(
            serde_json::to_string(&FinalPassSkip::NoModel).unwrap(),
            "\"no_engine\""
        );
        assert_eq!(
            serde_json::to_string(&FinalPassSkip::Disabled).unwrap(),
            "\"disabled\""
        );
        assert_eq!(
            serde_json::to_string(&FinalPassSkip::Edited).unwrap(),
            "\"edited\""
        );
    }

    #[test]
    fn timing_defaults_to_when_plugged_in_and_keeps_an_old_off_switch() {
        assert_eq!(FinalPassTiming::from_stored(None, None), WhenPluggedIn);
        assert_eq!(
            FinalPassTiming::from_stored(None, Some(true)),
            WhenPluggedIn
        );
        assert_eq!(FinalPassTiming::from_stored(None, Some(false)), Off);
        assert_eq!(
            FinalPassTiming::from_stored(Some("bogus"), None),
            WhenPluggedIn
        );
        for timing in [Automatic, WhenPluggedIn, Off] {
            // A stored choice wins over the old switch either way.
            assert_eq!(
                FinalPassTiming::from_stored(Some(timing.as_str()), Some(false)),
                timing
            );
            let json = serde_json::to_string(&timing).unwrap();
            assert_eq!(json, format!("\"{}\"", timing.as_str()));
        }
    }
}
