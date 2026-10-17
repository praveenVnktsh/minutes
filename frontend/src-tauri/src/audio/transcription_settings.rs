//! Live and final transcription model settings, their migration, and the
//! engine choice (with fallback) for each.
//!
//! - The **live** model streams text during a recording. The default is
//!   Nemotron streaming; `chunked` runs the final engine on VAD-cut chunks
//!   instead (the pre-split behaviour).
//! - The **final** model re-transcribes the whole recording after the meeting
//!   and replaces the live transcript. It is the SQLite transcript config row
//!   (provider + model) that retranscription already used.
//!
//! Everything that decides something is a pure function here so it can be
//! unit tested; the async wrappers only gather inputs and apply results.

use log::{info, warn};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_store::StoreExt;

use crate::config::{DEFAULT_PARAKEET_MODEL, DEFAULT_WHISPER_MODEL};

const STORE_FILE: &str = "store.json";
pub const LIVE_MODEL_KEY: &str = "liveTranscriptionModel";
pub const AUTO_FINAL_PASS_KEY: &str = "autoFinalTranscriptPass";
/// When the final pass runs: [`FinalPassTiming`]. Replaces the on/off
/// [`AUTO_FINAL_PASS_KEY`], which is still read when this key is absent and
/// still written alongside it.
pub const FINAL_PASS_TIMING_KEY: &str = "finalTranscriptTiming";
pub const FINAL_MODEL_MIGRATED_KEY: &str = "finalTranscriptModelMigrated";

pub const PROVIDER_WHISPER: &str = "localWhisper";
pub const PROVIDER_PARAKEET: &str = "parakeet";

/// Which family streams text during a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LiveModelChoice {
    /// Nemotron Speech Streaming 0.6B through sherpa-onnx (default).
    NemotronStreaming,
    /// The final engine, run on VAD-cut chunks.
    Chunked,
}

impl LiveModelChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NemotronStreaming => "nemotron-streaming",
            Self::Chunked => "chunked",
        }
    }

    /// Unset or unrecognised values mean the default, so a user who never
    /// picked a live model gets Nemotron.
    pub fn from_stored(value: Option<&str>) -> Self {
        match value {
            Some("chunked") => Self::Chunked,
            _ => Self::NemotronStreaming,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveTranscriptionSettings {
    pub live_model: LiveModelChoice,
    pub auto_final_pass: bool,
}

impl LiveTranscriptionSettings {
    pub fn from_stored(live_model: Option<&str>, auto_final_pass: Option<bool>) -> Self {
        Self {
            live_model: LiveModelChoice::from_stored(live_model),
            auto_final_pass: auto_final_pass.unwrap_or(true),
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

/// A transcription engine and model, as stored in the transcript config row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineModel {
    pub provider: String,
    pub model: String,
}

impl EngineModel {
    pub fn new(provider: &str, model: &str) -> Self {
        Self {
            provider: provider.to_string(),
            model: model.to_string(),
        }
    }

    pub fn default_final() -> Self {
        Self::new(PROVIDER_WHISPER, DEFAULT_WHISPER_MODEL)
    }

    pub fn is_parakeet(&self) -> bool {
        self.provider == PROVIDER_PARAKEET
    }

    /// Short name for user-facing messages.
    pub fn display_name(&self) -> String {
        let family = if self.is_parakeet() {
            "Parakeet"
        } else {
            "Whisper"
        };
        format!("{} {}", family, self.model)
    }
}

/// The one-time final-model migration. Returns the config to write, or `None`
/// to leave the stored row alone.
///
/// Onboarding always saved Parakeet v2, so that row cannot be told apart from
/// an explicit choice; it is treated as the old default and moved to the new
/// default (Whisper turbo, which is the only engine the custom vocabulary
/// reaches). Any other stored choice is the user's and is kept.
pub fn migrate_final_model(
    stored: Option<&EngineModel>,
    already_migrated: bool,
) -> Option<EngineModel> {
    if already_migrated {
        return None;
    }
    match stored {
        None => Some(EngineModel::default_final()),
        Some(config) if config.is_parakeet() && config.model == DEFAULT_PARAKEET_MODEL => {
            Some(EngineModel::default_final())
        }
        Some(_) => None,
    }
}

/// Downloaded models per engine, in catalog order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Availability {
    pub whisper: Vec<String>,
    pub parakeet: Vec<String>,
}

impl Availability {
    fn has(&self, engine: &EngineModel) -> bool {
        let list = if engine.is_parakeet() {
            &self.parakeet
        } else {
            &self.whisper
        };
        list.iter().any(|model| model == &engine.model)
    }

    pub fn is_empty(&self) -> bool {
        self.whisper.is_empty() && self.parakeet.is_empty()
    }
}

/// The engine a final pass (or chunked live transcription) will use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineChoice {
    pub engine: EngineModel,
    /// Set when `engine` is not the preferred one, saying why.
    pub fallback_notice: Option<String>,
}

/// Rank Whisper models for fallback: larger and newer first.
fn whisper_rank(model: &str) -> u8 {
    let checks: [(&str, u8); 6] = [
        ("large-v3-turbo", 0),
        ("large-v3", 1),
        ("large", 2),
        ("medium", 3),
        ("small", 4),
        ("base", 5),
    ];
    checks
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map(|(_, rank)| *rank)
        .unwrap_or(6)
}

/// Pick the engine for `preferred`, falling back to the best downloaded one.
/// Order: preferred → Whisper turbo → largest other Whisper → Parakeet v2 →
/// any Parakeet. `None` when nothing is downloaded.
pub fn choose_final_engine(
    preferred: &EngineModel,
    available: &Availability,
) -> Option<EngineChoice> {
    if available.has(preferred) {
        return Some(EngineChoice {
            engine: preferred.clone(),
            fallback_notice: None,
        });
    }

    let mut whisper: Vec<&String> = available.whisper.iter().collect();
    whisper.sort_by_key(|model| whisper_rank(model));
    let fallback = whisper
        .first()
        .map(|model| EngineModel::new(PROVIDER_WHISPER, model))
        .or_else(|| {
            available
                .parakeet
                .iter()
                .find(|model| model.as_str() == DEFAULT_PARAKEET_MODEL)
                .or_else(|| available.parakeet.first())
                .map(|model| EngineModel::new(PROVIDER_PARAKEET, model))
        })?;

    Some(EngineChoice {
        fallback_notice: Some(format!(
            "{} is not downloaded, so {} was used instead. Download it in Settings → Transcription.",
            preferred.display_name(),
            fallback.display_name()
        )),
        engine: fallback,
    })
}

/// How live transcription runs for one recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LivePlan {
    /// Nemotron streaming on continuous audio.
    Streaming,
    /// An offline engine on VAD-cut chunks.
    Chunked(EngineChoice),
    /// No engine is available; record without live text.
    Unavailable { notice: String },
}

/// Decide the live plan. Never fails: a missing model degrades to chunked or
/// to no live text, with a notice for the user, but recording goes on.
pub fn resolve_live_plan(
    choice: LiveModelChoice,
    nemotron_ready: bool,
    final_model: &EngineModel,
    available: &Availability,
) -> LivePlan {
    if choice == LiveModelChoice::NemotronStreaming && nemotron_ready {
        return LivePlan::Streaming;
    }
    match choose_final_engine(final_model, available) {
        Some(mut chunked) => {
            if choice == LiveModelChoice::NemotronStreaming {
                chunked.fallback_notice = Some(format!(
                    "The Nemotron live model is not ready yet, so live text uses {} for this meeting.",
                    chunked.engine.display_name()
                ));
            }
            LivePlan::Chunked(chunked)
        }
        None => LivePlan::Unavailable {
            notice: "No transcription model is downloaded yet, so this meeting is recorded without live text. It will be transcribed once a model is available.".to_string(),
        },
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
    /// No Whisper or Parakeet model is downloaded.
    NoEngine,
}

impl FinalPassSkip {
    pub fn message(self) -> &'static str {
        match self {
            Self::Disabled => "The automatic final transcript is turned off.",
            Self::Edited => "The transcript has edits, so it was not replaced by a final pass.",
            Self::NoEngine => "No final transcript model is downloaded. Download one in Settings → Transcription, then retranscribe the meeting.",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalPassDecision {
    Run(EngineChoice),
    /// Run it once the Mac is plugged in; the live transcript stands until then.
    WaitForPower(EngineChoice),
    Skip(FinalPassSkip),
}

/// Whether the final pass runs, and with which engine.
/// - Edits always win: a transcript with revisions is never replaced.
/// - `force` (deferred mode, or the user asking for it) and an empty live
///   transcript (no live engine was available) run now, whatever the timing
///   and the battery, since otherwise the meeting would have no transcript
///   or the user would be kept waiting.
/// - Otherwise "When plugged in" waits while `saving_power` (on battery or
///   in Low Power Mode, see `quiet_work::saving_power_now`).
pub fn decide_final_pass(
    timing: FinalPassTiming,
    saving_power: bool,
    force: bool,
    has_revisions: bool,
    has_transcripts: bool,
    engine: Option<EngineChoice>,
) -> FinalPassDecision {
    if has_revisions {
        return FinalPassDecision::Skip(FinalPassSkip::Edited);
    }
    let urgent = force || !has_transcripts;
    if timing == FinalPassTiming::Off && !urgent {
        return FinalPassDecision::Skip(FinalPassSkip::Disabled);
    }
    let Some(engine) = engine else {
        return FinalPassDecision::Skip(FinalPassSkip::NoEngine);
    };
    if timing == FinalPassTiming::WhenPluggedIn && saving_power && !urgent {
        return FinalPassDecision::WaitForPower(engine);
    }
    FinalPassDecision::Run(engine)
}

// ---------------------------------------------------------------------------
// App wrappers
// ---------------------------------------------------------------------------

pub fn load_settings<R: Runtime>(app: &AppHandle<R>) -> LiveTranscriptionSettings {
    match app.store(STORE_FILE) {
        Ok(store) => LiveTranscriptionSettings::from_stored(
            store.get(LIVE_MODEL_KEY).as_ref().and_then(|v| v.as_str()),
            store
                .get(AUTO_FINAL_PASS_KEY)
                .as_ref()
                .and_then(|v| v.as_bool()),
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

/// Downloaded Whisper and Parakeet models. Initialises the engines (cheap,
/// no model load) so discovery can run.
pub async fn availability() -> Availability {
    let mut available = Availability::default();

    if crate::whisper_engine::commands::whisper_init()
        .await
        .is_ok()
    {
        let engine = crate::whisper_engine::commands::WHISPER_ENGINE
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned());
        if let Some(engine) = engine {
            match engine.discover_models().await {
                Ok(models) => {
                    available.whisper = models
                        .into_iter()
                        .filter(|m| {
                            matches!(m.status, crate::whisper_engine::ModelStatus::Available)
                        })
                        .map(|m| m.name)
                        .collect()
                }
                Err(error) => warn!("Whisper model discovery failed: {error}"),
            }
        }
    }

    if crate::parakeet_engine::commands::parakeet_init()
        .await
        .is_ok()
    {
        let engine = crate::parakeet_engine::commands::PARAKEET_ENGINE
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned());
        if let Some(engine) = engine {
            match engine.discover_models().await {
                Ok(models) => {
                    available.parakeet = models
                        .into_iter()
                        .filter(|m| {
                            matches!(m.status, crate::parakeet_engine::ModelStatus::Available)
                        })
                        .map(|m| m.name)
                        .collect()
                }
                Err(error) => warn!("Parakeet model discovery failed: {error}"),
            }
        }
    }

    available
}

/// The configured final model (the transcript config row), or the default.
pub async fn configured_final_model<R: Runtime>(app: &AppHandle<R>) -> EngineModel {
    let Some(state) = app.try_state::<crate::state::AppState>() else {
        return EngineModel::default_final();
    };
    match crate::database::repositories::setting::SettingsRepository::get_transcript_config(
        state.db_manager.pool(),
    )
    .await
    {
        Ok(Some(config))
            if config.provider == PROVIDER_PARAKEET || config.provider == PROVIDER_WHISPER =>
        {
            EngineModel::new(&config.provider, &config.model)
        }
        Ok(_) => EngineModel::default_final(),
        Err(error) => {
            warn!("Could not read the final transcript model, using the default: {error}");
            EngineModel::default_final()
        }
    }
}

/// Run the one-time final-model migration. Safe to call more than once.
pub async fn migrate_settings<R: Runtime>(app: &AppHandle<R>) {
    let Ok(store) = app.store(STORE_FILE) else {
        return;
    };
    let migrated = store
        .get(FINAL_MODEL_MIGRATED_KEY)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let Some(state) = app.try_state::<crate::state::AppState>() else {
        return;
    };
    let pool = state.db_manager.pool();
    let stored =
        match crate::database::repositories::setting::SettingsRepository::get_transcript_config(
            pool,
        )
        .await
        {
            Ok(row) => row.map(|config| EngineModel::new(&config.provider, &config.model)),
            Err(error) => {
                warn!("Final model migration skipped, transcript config unreadable: {error}");
                return;
            }
        };
    if let Some(target) = migrate_final_model(stored.as_ref(), migrated) {
        match crate::database::repositories::setting::SettingsRepository::save_transcript_config(
            pool,
            &target.provider,
            &target.model,
        )
        .await
        {
            Ok(()) => info!(
                "Final transcript model migrated from {:?} to {}/{}",
                stored, target.provider, target.model
            ),
            Err(error) => {
                warn!("Final model migration failed, will retry next start: {error}");
                return;
            }
        }
    }
    if !migrated {
        store.set(FINAL_MODEL_MIGRATED_KEY, serde_json::json!(true));
        if let Err(error) = store.save() {
            warn!("Could not persist the final model migration flag: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whisper(model: &str) -> EngineModel {
        EngineModel::new(PROVIDER_WHISPER, model)
    }

    fn parakeet(model: &str) -> EngineModel {
        EngineModel::new(PROVIDER_PARAKEET, model)
    }

    #[test]
    fn unset_settings_default_to_nemotron_and_auto_final_pass() {
        let settings = LiveTranscriptionSettings::from_stored(None, None);
        assert_eq!(settings.live_model, LiveModelChoice::NemotronStreaming);
        assert!(settings.auto_final_pass);
    }

    #[test]
    fn explicit_settings_are_respected_and_unknown_values_fall_back() {
        let settings = LiveTranscriptionSettings::from_stored(Some("chunked"), Some(false));
        assert_eq!(settings.live_model, LiveModelChoice::Chunked);
        assert!(!settings.auto_final_pass);
        assert_eq!(
            LiveModelChoice::from_stored(Some("something-old")),
            LiveModelChoice::NemotronStreaming
        );
    }

    #[test]
    fn live_choice_round_trips_through_serde() {
        let json = serde_json::to_string(&LiveModelChoice::NemotronStreaming).unwrap();
        assert_eq!(json, "\"nemotron-streaming\"");
        assert_eq!(
            LiveModelChoice::from_stored(Some(LiveModelChoice::Chunked.as_str())),
            LiveModelChoice::Chunked
        );
    }

    #[test]
    fn migration_moves_missing_or_onboarding_default_to_whisper_turbo() {
        assert_eq!(
            migrate_final_model(None, false),
            Some(EngineModel::default_final())
        );
        assert_eq!(
            migrate_final_model(Some(&parakeet(DEFAULT_PARAKEET_MODEL)), false),
            Some(EngineModel::default_final())
        );
    }

    #[test]
    fn migration_keeps_explicit_choices_and_runs_once() {
        assert_eq!(
            migrate_final_model(Some(&whisper("large-v3-q5_0")), false),
            None
        );
        assert_eq!(
            migrate_final_model(Some(&parakeet("parakeet-tdt-0.6b-v3-int8")), false),
            None
        );
        assert_eq!(migrate_final_model(None, true), None);
        assert_eq!(
            migrate_final_model(Some(&parakeet(DEFAULT_PARAKEET_MODEL)), true),
            None
        );
    }

    #[test]
    fn preferred_engine_is_used_when_downloaded() {
        let available = Availability {
            whisper: vec![DEFAULT_WHISPER_MODEL.into()],
            parakeet: vec![DEFAULT_PARAKEET_MODEL.into()],
        };
        let choice = choose_final_engine(&EngineModel::default_final(), &available).unwrap();
        assert_eq!(choice.engine, EngineModel::default_final());
        assert!(choice.fallback_notice.is_none());
    }

    #[test]
    fn missing_whisper_falls_back_to_parakeet_with_a_notice() {
        let available = Availability {
            whisper: vec![],
            parakeet: vec![
                "parakeet-tdt-0.6b-v3-int8".into(),
                DEFAULT_PARAKEET_MODEL.into(),
            ],
        };
        let choice = choose_final_engine(&EngineModel::default_final(), &available).unwrap();
        assert_eq!(choice.engine, parakeet(DEFAULT_PARAKEET_MODEL));
        let notice = choice.fallback_notice.unwrap();
        assert!(notice.contains("not downloaded"), "{notice}");
    }

    #[test]
    fn fallback_prefers_the_largest_downloaded_whisper() {
        let available = Availability {
            whisper: vec![
                "small-q5_1".into(),
                "large-v3-q5_0".into(),
                "medium-q5_0".into(),
            ],
            parakeet: vec![DEFAULT_PARAKEET_MODEL.into()],
        };
        let choice = choose_final_engine(&EngineModel::default_final(), &available).unwrap();
        assert_eq!(choice.engine, whisper("large-v3-q5_0"));
    }

    #[test]
    fn nothing_downloaded_means_no_engine() {
        assert!(
            choose_final_engine(&EngineModel::default_final(), &Availability::default()).is_none()
        );
    }

    #[test]
    fn live_plan_streams_when_nemotron_is_ready() {
        let plan = resolve_live_plan(
            LiveModelChoice::NemotronStreaming,
            true,
            &EngineModel::default_final(),
            &Availability::default(),
        );
        assert_eq!(plan, LivePlan::Streaming);
    }

    #[test]
    fn live_plan_falls_back_to_chunked_with_a_notice_when_nemotron_is_missing() {
        let available = Availability {
            whisper: vec![],
            parakeet: vec![DEFAULT_PARAKEET_MODEL.into()],
        };
        let plan = resolve_live_plan(
            LiveModelChoice::NemotronStreaming,
            false,
            &EngineModel::default_final(),
            &available,
        );
        match plan {
            LivePlan::Chunked(choice) => {
                assert_eq!(choice.engine, parakeet(DEFAULT_PARAKEET_MODEL));
                assert!(choice.fallback_notice.unwrap().contains("Nemotron"));
            }
            other => panic!("expected chunked, got {other:?}"),
        }
    }

    #[test]
    fn explicit_chunked_choice_ignores_nemotron_and_carries_no_live_notice() {
        let available = Availability {
            whisper: vec![DEFAULT_WHISPER_MODEL.into()],
            parakeet: vec![],
        };
        let plan = resolve_live_plan(
            LiveModelChoice::Chunked,
            true,
            &EngineModel::default_final(),
            &available,
        );
        assert_eq!(
            plan,
            LivePlan::Chunked(EngineChoice {
                engine: EngineModel::default_final(),
                fallback_notice: None
            })
        );
    }

    fn turbo_choice() -> Option<EngineChoice> {
        Some(EngineChoice {
            engine: EngineModel::default_final(),
            fallback_notice: None,
        })
    }

    use FinalPassTiming::{Automatic, Off, WhenPluggedIn};

    #[test]
    fn final_pass_runs_by_default_after_a_live_transcript_on_power() {
        for timing in [Automatic, WhenPluggedIn] {
            assert_eq!(
                decide_final_pass(timing, false, false, false, true, turbo_choice()),
                FinalPassDecision::Run(turbo_choice().unwrap())
            );
        }
    }

    #[test]
    fn on_battery_the_default_waits_for_power_and_automatic_does_not() {
        assert_eq!(
            decide_final_pass(WhenPluggedIn, true, false, false, true, turbo_choice()),
            FinalPassDecision::WaitForPower(turbo_choice().unwrap())
        );
        assert_eq!(
            decide_final_pass(Automatic, true, false, false, true, turbo_choice()),
            FinalPassDecision::Run(turbo_choice().unwrap())
        );
    }

    #[test]
    fn asking_for_it_or_having_no_live_transcript_never_waits() {
        // The user pressed "Improve now" (or deferred mode forced it).
        assert!(matches!(
            decide_final_pass(WhenPluggedIn, true, true, false, true, turbo_choice()),
            FinalPassDecision::Run(_)
        ));
        // Without a live transcript the meeting would have no text at all.
        assert!(matches!(
            decide_final_pass(WhenPluggedIn, true, false, false, false, turbo_choice()),
            FinalPassDecision::Run(_)
        ));
    }

    #[test]
    fn final_pass_never_replaces_an_edited_transcript_even_when_forced() {
        assert_eq!(
            decide_final_pass(Automatic, false, true, true, true, turbo_choice()),
            FinalPassDecision::Skip(FinalPassSkip::Edited)
        );
    }

    #[test]
    fn disabled_final_pass_still_runs_when_forced_or_without_a_live_transcript() {
        for on_battery in [false, true] {
            assert_eq!(
                decide_final_pass(Off, on_battery, false, false, true, turbo_choice()),
                FinalPassDecision::Skip(FinalPassSkip::Disabled)
            );
        }
        assert!(matches!(
            decide_final_pass(Off, true, true, false, true, turbo_choice()),
            FinalPassDecision::Run(_)
        ));
        assert!(matches!(
            decide_final_pass(Off, false, false, false, false, turbo_choice()),
            FinalPassDecision::Run(_)
        ));
    }

    #[test]
    fn final_pass_without_any_engine_is_skipped_with_a_reason() {
        for on_battery in [false, true] {
            assert_eq!(
                decide_final_pass(WhenPluggedIn, on_battery, false, false, true, None),
                FinalPassDecision::Skip(FinalPassSkip::NoEngine)
            );
        }
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

    #[test]
    fn live_plan_never_fails_without_any_model() {
        let plan = resolve_live_plan(
            LiveModelChoice::NemotronStreaming,
            false,
            &EngineModel::default_final(),
            &Availability::default(),
        );
        assert!(matches!(plan, LivePlan::Unavailable { .. }));
    }
}
