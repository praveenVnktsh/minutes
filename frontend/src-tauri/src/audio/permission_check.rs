// audio/permission_check.rs
//
// One verifier per permission, shared by onboarding and by the start of a
// recording so the two surfaces cannot disagree about the same grant. They used
// to check separately and differently: onboarding built a stream, slept 500 ms
// and called the permission granted, while recording-start waited for an audio
// callback and refused to start without one. Setup could therefore certify
// exactly what recording would go on to reject.
//
// Every verdict here is drawn from audio observed arriving — a microphone
// callback that fired carrying a non-zero sample, or a tap sample that was not
// silence. Constructing a stream or a tap proves nothing: macOS hands a denied
// app a stream that opens cleanly and then stays silent, which is what made the
// old checks wrong rather than merely weak.
//
// The third verdict is the whole point of the tri-state. A check that could not
// be made — no input device, a platform where system audio is not captured at
// all, a tap that was silent because nothing was playing — is reported as
// undetermined, never as a grant and never as a denial. Either guess sends a
// user somewhere they did not need to go.
//
// What silence means, per channel and per platform, is not decided here a
// second time: `mic_check.rs` documents the same rules for the setup check, and
// the comments below say so wherever they restate one.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Stream, SupportedStreamConfig};
use log::{info, warn};
use serde::{Deserialize, Serialize};

use super::capture_probe::peak_of;

/// How long the microphone is given to produce audio. Taken from the
/// record-start check this module supersedes: a granted microphone answers in
/// well under 100 ms, so five seconds is generous room for a slow device rather
/// than an expected wait.
const MICROPHONE_WAIT: Duration = Duration::from_secs(5);

/// How often the wait inspects what the audio callback has accumulated. Short
/// enough that the happy path returns promptly, long enough that the poll costs
/// nothing next to the capture it is watching.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The tap is given less time than the microphone because there is nothing to
/// wait for beyond the first buffer: no verdict improves by listening longer to
/// a channel that may legitimately be silent.
#[cfg(target_os = "macos")]
const SYSTEM_AUDIO_WAIT: Duration = Duration::from_secs(2);

/// Used when the device is open but will not name itself, so a detail line
/// still reads as a sentence.
const UNNAMED_DEVICE: &str = "the default microphone";

/// The sentence recording-start and onboarding both show a user whose
/// microphone came back denied. One copy, so the two surfaces cannot drift.
///
/// It names no platform's settings app: this check now runs on Windows and
/// Linux too, where the macOS wording the record-start check used to print
/// would be a direction to a screen that does not exist. It also names the
/// muted input, because on macOS an all-zero capture is read as a denial and a
/// muted microphone produces exactly that — the message must not insist on a
/// permission problem the check cannot distinguish.
pub const MICROPHONE_DENIED_MESSAGE: &str =
    "No audio reached the app from the microphone. Allow microphone access for this app in \
     your system's privacy settings, check the input is not muted, then try again.";

/// What one permission check concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionVerdict {
    /// Audio was observed arriving. Nothing else reaches this.
    Authorized,
    /// The channel ran and the operating system withheld the audio.
    Denied,
    /// The check could not be made, so neither of the other two is honest.
    Undetermined,
}

/// The verdict plus the evidence behind it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionReport {
    pub verdict: PermissionVerdict,
    /// Why, for the log and for a details line. Never the sentence shown to the
    /// user on its own — the verdict decides that.
    pub detail: Option<String>,
}

impl PermissionReport {
    pub fn authorized(detail: Option<String>) -> Self {
        Self {
            verdict: PermissionVerdict::Authorized,
            detail,
        }
    }

    pub fn denied(detail: impl Into<String>) -> Self {
        Self {
            verdict: PermissionVerdict::Denied,
            detail: Some(detail.into()),
        }
    }

    pub fn undetermined(detail: impl Into<String>) -> Self {
        Self {
            verdict: PermissionVerdict::Undetermined,
            detail: Some(detail.into()),
        }
    }

    pub fn is_authorized(&self) -> bool {
        matches!(self.verdict, PermissionVerdict::Authorized)
    }

    pub fn is_denied(&self) -> bool {
        matches!(self.verdict, PermissionVerdict::Denied)
    }
}

/// What the microphone stream was seen to do, which is all the verdict is drawn
/// from. The callback count and the peak answer different questions — a device
/// that never called back and a device that called back with digital silence
/// are different faults — so both are kept.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct MicrophoneEvidence {
    callbacks: u64,
    peak: f32,
}

/// Verify the microphone by observing audio, never by building a stream.
///
/// Opening the stream also puts the system permission prompt on screen when the
/// state was never determined, which is why this is the same call onboarding
/// makes: the prompt and the verification are one act, and the answer is
/// whatever audio does or does not arrive afterwards.
pub async fn verify_microphone() -> PermissionReport {
    // cpal's `Stream` is not `Send` on every platform, so the whole capture
    // lives and dies on one blocking thread and only the verdict comes back.
    let report = match tokio::task::spawn_blocking(observe_microphone).await {
        Ok(report) => report,
        Err(error) => PermissionReport::undetermined(format!(
            "The microphone check did not finish: {}",
            error
        )),
    };

    info!(
        "Microphone permission check concluded {:?}: {}",
        report.verdict,
        report.detail.as_deref().unwrap_or("no further detail")
    );
    report
}

/// Open the default input, watch it for up to [`MICROPHONE_WAIT`], and report
/// what it did.
fn observe_microphone() -> PermissionReport {
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        // A machine with no input at all has no microphone permission to
        // report on. This is the headless case, and calling it a denial would
        // invent a fault the user cannot fix.
        return PermissionReport::undetermined("No input device is available to check");
    };

    let device_name = device.name().unwrap_or_else(|_| UNNAMED_DEVICE.to_string());

    let config = match device.default_input_config() {
        Ok(config) => config,
        Err(error) => {
            return classify_stream_error(
                format!(
                    "Could not read the input configuration of '{}': {}",
                    device_name, error
                ),
                &error.to_string(),
            )
        }
    };

    let evidence = Arc::new(Mutex::new(MicrophoneEvidence::default()));
    let stream = match build_observation_stream(&device, &config, evidence.clone()) {
        Ok(stream) => stream,
        Err(error) => {
            return classify_stream_error(
                format!("Could not open '{}': {}", device_name, error),
                &error.to_string(),
            )
        }
    };
    if let Err(error) = stream.play() {
        return classify_stream_error(
            format!("Could not start '{}': {}", device_name, error),
            &error.to_string(),
        );
    }

    let started = Instant::now();
    let observed = loop {
        let observed = *evidence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // A granted microphone delivers its first non-zero sample in well under
        // 100 ms, so returning the moment one lands keeps the happy path fast
        // and leaves the full wait for the cases that actually need it.
        if observed.peak > 0.0 || started.elapsed() >= MICROPHONE_WAIT {
            break observed;
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    // Pause before dropping so the callback stops touching the evidence first.
    if let Err(error) = stream.pause() {
        warn!(
            "Could not pause the '{}' permission check stream cleanly: {}",
            device_name, error
        );
    }
    drop(stream);

    microphone_verdict(observed, &device_name)
}

/// The verdict the microphone evidence alone supports.
///
/// Kept free of every device so it can be tested without hardware, which is
/// also the only way these rules are covered at all: CI has no audio.
fn microphone_verdict(evidence: MicrophoneEvidence, device_name: &str) -> PermissionReport {
    // A stream that opened, ran for the whole window and delivered not one
    // callback is the operating system withholding the audio, on every platform
    // and not only macOS: a quiet room still produces samples. `mic_check.rs`
    // documents the same rule on `no_capture_outcome`.
    if evidence.callbacks == 0 {
        return PermissionReport::denied(format!(
            "'{}' delivered no audio callbacks in {} seconds",
            device_name,
            MICROPHONE_WAIT.as_secs()
        ));
    }

    if evidence.peak > 0.0 {
        return PermissionReport::authorized(Some(format!(
            "'{}' delivered audio (peak {:.4})",
            device_name, evidence.peak
        )));
    }

    // Callbacks that carry nothing but bit-exact zeroes. macOS hands a denied
    // app a stream of digital silence rather than refusing to open the
    // microphone, so zeroes there indict the permission; everywhere else they
    // are a muted or wrong input and nothing to do with a grant.
    // `mic_check.rs` documents the same rule on `digital_silence_outcome`.
    #[cfg(target_os = "macos")]
    {
        PermissionReport::denied(format!(
            "'{}' delivered {} callbacks of digital silence, which is what macOS gives an app \
             that was denied the microphone",
            device_name, evidence.callbacks
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        PermissionReport::authorized(Some(format!(
            "'{}' delivered audio, though every sample was silent; on this platform that is a \
             muted or wrong input rather than a permission problem",
            device_name
        )))
    }
}

/// Verify system audio the same way.
#[cfg(target_os = "macos")]
pub async fn verify_system_audio() -> PermissionReport {
    use futures_util::StreamExt;

    // Creating the tap is what raises the Audio Capture prompt, and it is also
    // the only thing the check it replaces ever did. A tap that exists is not
    // evidence of anything; what it yields is.
    let mut stream = match crate::audio::capture::start_system_audio_capture().await {
        Ok(stream) => stream,
        Err(error) => {
            let report = classify_stream_error(
                format!("The system audio tap could not be started: {}", error),
                &error.to_string(),
            );
            info!(
                "System audio permission check concluded {:?}: {}",
                report.verdict,
                report.detail.as_deref().unwrap_or("no further detail")
            );
            return report;
        }
    };

    // Timing out the whole loop is what bounds the wait: the tap future is
    // dropped at the await it was parked on, whatever is still pending.
    let heard_audio = tokio::time::timeout(SYSTEM_AUDIO_WAIT, async {
        while let Some(sample) = stream.next().await {
            if sample != 0.0 {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    let report = system_audio_verdict(heard_audio);
    info!(
        "System audio permission check concluded {:?}: {}",
        report.verdict,
        report.detail.as_deref().unwrap_or("no further detail")
    );
    report
}

/// Verify system audio the same way.
///
/// There is no system audio to verify off macOS: `start_system_audio_capture`
/// bails with "not yet implemented for this platform" before any permission
/// could be involved. The check this replaces returned a grant here, which was
/// a claim about a channel the app cannot open at all.
#[cfg(not(target_os = "macos"))]
pub async fn verify_system_audio() -> PermissionReport {
    let report = PermissionReport::undetermined(
        "System audio capture is not implemented on this platform, so there is no permission to \
         verify",
    );
    info!(
        "System audio permission check concluded {:?}: {}",
        report.verdict,
        report.detail.as_deref().unwrap_or("no further detail")
    );
    report
}

/// The verdict a run of the tap supports.
///
/// Compiled off macOS as well so its rule is covered by the tests on every
/// platform CI runs.
#[cfg(any(target_os = "macos", test))]
fn system_audio_verdict(heard_audio: bool) -> PermissionReport {
    if heard_audio {
        return PermissionReport::authorized(Some(
            "The system audio tap delivered a non-silent sample".to_string(),
        ));
    }

    // Deliberately not a denial. A Mac with nothing playing produces exactly
    // this, and so does a Mac whose Audio Capture grant is missing, and the tap
    // gives no way to tell them apart — `mic_check.rs` makes the same point on
    // `digital_silence_outcome`, where calling it a permission problem would
    // send a user whose setup is fine into System Settings.
    PermissionReport::undetermined(
        "The system audio tap was silent, which is what both a missing grant and a Mac with \
         nothing playing produce, so this check cannot tell them apart",
    )
}

/// What a failure to set up a stream says about the permission.
///
/// On its own, nothing: a device can refuse to open for a dozen reasons that
/// have no permission in them. The exception is an operating system that says
/// so in as many words, which is the only case read as a denial. No branch here
/// may reach `Authorized` — a stream that never ran observed no audio.
fn classify_stream_error(detail: String, error: &str) -> PermissionReport {
    let lowered = error.to_lowercase();
    if lowered.contains("permission") || lowered.contains("denied") {
        return PermissionReport::denied(detail);
    }

    PermissionReport::undetermined(detail)
}

fn build_observation_stream(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    evidence: Arc<Mutex<MicrophoneEvidence>>,
) -> anyhow::Result<Stream> {
    let stream_config: cpal::StreamConfig = config.clone().into();
    let on_error = move |error: cpal::StreamError| {
        warn!("Microphone permission check stream error: {}", error)
    };

    // Every sample format the device might hand back is accepted: this check now
    // runs on Windows and Linux, where an input whose native format is not f32
    // is ordinary, and a stream that failed to build for that reason would be
    // reported as an undetermined permission it has nothing to do with.
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                observe_samples(&evidence, data.iter().copied());
            },
            on_error,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                observe_samples(
                    &evidence,
                    data.iter().map(|&sample| sample as f32 / i16::MAX as f32),
                );
            },
            on_error,
            None,
        )?,
        cpal::SampleFormat::I32 => device.build_input_stream(
            &stream_config,
            move |data: &[i32], _: &cpal::InputCallbackInfo| {
                observe_samples(
                    &evidence,
                    data.iter().map(|&sample| sample as f32 / i32::MAX as f32),
                );
            },
            on_error,
            None,
        )?,
        cpal::SampleFormat::I8 => device.build_input_stream(
            &stream_config,
            move |data: &[i8], _: &cpal::InputCallbackInfo| {
                observe_samples(
                    &evidence,
                    data.iter().map(|&sample| sample as f32 / i8::MAX as f32),
                );
            },
            on_error,
            None,
        )?,
        format => return Err(anyhow::anyhow!("Unsupported sample format: {:?}", format)),
    };

    Ok(stream)
}

/// The audio callback records only what it saw; the verdict is decided once the
/// wait is over. The window is measured with `peak_of`, the same arithmetic the
/// setup check meters with, so "non-zero" means the same thing in both.
fn observe_samples(evidence: &Mutex<MicrophoneEvidence>, data: impl IntoIterator<Item = f32>) {
    let samples: Vec<f32> = data.into_iter().collect();
    let mut evidence = evidence
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    evidence.callbacks += 1;
    evidence.peak = evidence.peak.max(peak_of(&samples));
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE: &str = "MacBook Pro Microphone";

    fn evidence(callbacks: u64, peak: f32) -> MicrophoneEvidence {
        MicrophoneEvidence { callbacks, peak }
    }

    #[test]
    fn a_microphone_that_handed_back_nothing_is_denied_on_every_platform() {
        // A quiet room still produces samples, so a stream that ran the whole
        // window without one callback is the OS holding the audio back;
        // mic_check.rs documents the same rule for the setup check.
        let report = microphone_verdict(evidence(0, 0.0), DEVICE);

        assert_eq!(report.verdict, PermissionVerdict::Denied);
        assert!(report.is_denied());
        assert!(!report.is_authorized());
        assert!(report.detail.unwrap().contains(DEVICE));
    }

    #[test]
    fn only_an_observed_sample_authorises_the_microphone() {
        let report = microphone_verdict(evidence(12, 0.42), DEVICE);

        assert_eq!(report.verdict, PermissionVerdict::Authorized);
        assert!(report.is_authorized());
        assert!(!report.is_denied());
    }

    #[test]
    fn microphone_digital_silence_indicts_the_permission_on_macos_only() {
        // The platform split is mic_check.rs's, not a second opinion: macOS
        // hands a denied app zeroes, everywhere else zeroes are a muted input.
        let report = microphone_verdict(evidence(40, 0.0), DEVICE);

        #[cfg(target_os = "macos")]
        assert_eq!(report.verdict, PermissionVerdict::Denied);

        #[cfg(not(target_os = "macos"))]
        assert_eq!(report.verdict, PermissionVerdict::Authorized);
    }

    #[test]
    fn a_silent_system_audio_tap_cannot_tell_a_denial_from_a_quiet_mac() {
        let report = system_audio_verdict(false);

        assert_eq!(report.verdict, PermissionVerdict::Undetermined);
        assert!(!report.is_authorized());
        assert!(!report.is_denied());
    }

    #[test]
    fn system_audio_is_authorised_only_once_a_sample_arrives() {
        assert!(system_audio_verdict(true).is_authorized());
    }

    #[test]
    fn a_stream_failure_is_a_denial_only_when_the_os_says_so() {
        let denied = classify_stream_error(
            "Could not open the microphone".to_string(),
            "Permission denied by the operating system",
        );
        assert_eq!(denied.verdict, PermissionVerdict::Denied);

        let refused = classify_stream_error(
            "Could not open the microphone".to_string(),
            "The device was DENIED access",
        );
        assert_eq!(refused.verdict, PermissionVerdict::Denied);

        // A busy or missing device says nothing about the grant, and must never
        // be reported as one either way.
        let busy = classify_stream_error(
            "Could not open the microphone".to_string(),
            "Device is already in use",
        );
        assert_eq!(busy.verdict, PermissionVerdict::Undetermined);
        assert_eq!(busy.detail.unwrap(), "Could not open the microphone");
    }

    #[test]
    fn the_denied_message_holds_on_every_platform_this_check_now_runs_on() {
        // It is shown on Windows and Linux too, so it may not send anyone to a
        // macOS settings pane, and it has to allow for the muted input that
        // looks identical to a denial on macOS.
        assert!(!MICROPHONE_DENIED_MESSAGE.contains("System Settings"));
        assert!(!MICROPHONE_DENIED_MESSAGE.contains("Privacy & Security"));
        assert!(MICROPHONE_DENIED_MESSAGE.contains("muted"));
    }

    #[test]
    fn verdict_names_match_the_wire_contract() {
        // The frontend switches on these by name, so a variant renamed here is
        // a wrong row at runtime rather than a type error.
        assert_eq!(
            serde_json::to_value(PermissionVerdict::Authorized).unwrap(),
            "authorized"
        );
        assert_eq!(
            serde_json::to_value(PermissionVerdict::Denied).unwrap(),
            "denied"
        );
        assert_eq!(
            serde_json::to_value(PermissionVerdict::Undetermined).unwrap(),
            "undetermined"
        );
    }

    #[test]
    fn a_report_serialises_its_verdict_and_its_detail() {
        let json = serde_json::to_value(PermissionReport::denied("no callbacks arrived")).unwrap();
        assert_eq!(json["verdict"], "denied");
        assert_eq!(json["detail"], "no callbacks arrived");

        let json = serde_json::to_value(PermissionReport::authorized(None)).unwrap();
        assert_eq!(json["verdict"], "authorized");
        assert!(json["detail"].is_null());
    }
}
