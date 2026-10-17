//! Running post-meeting work quietly: at a background quality of service, so
//! macOS prefers the efficiency cores and throttles the work under load and
//! heat, and only when the Mac is plugged in if the user asked for that.
//!
//! - [`run_quietly`] runs a closure on a fresh thread at `QOS_CLASS_UTILITY`.
//!   A fresh thread, not the caller's: lowering a reused thread (a tokio
//!   worker or blocking-pool thread) would leave it low for whatever runs on
//!   it next. Threads a library starts from inside the closure (ONNX
//!   Runtime's pool, ggml's workers) do *not* inherit the class on macOS;
//!   only the calling thread, which submits the GPU work and takes part in
//!   the CPU work, is lowered.
//! - [`power_source`] reads whether the Mac runs on its battery.

use std::ffi::c_void;

/// The quality of service post-meeting work runs at. Utility rather than
/// background: background can stall for minutes under load, and the user is
/// usually waiting for the improved transcript, just not urgently.
const QUIET_QOS: libc::qos_class_t = libc::qos_class_t::QOS_CLASS_UTILITY;

/// Run `work` on its own thread at utility QoS and wait for it. Blocks the
/// caller, so call it from blocking code (or where the caller already
/// blocked, as Whisper's synchronous decode does).
pub fn run_quietly<T, F>(work: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("quiet-work".into())
            .spawn_scoped(scope, || {
                lower_current_thread();
                work()
            })
            .expect("could not start a thread for post-meeting work");
        match handle.join() {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

/// [`run_quietly`] from async code: on the blocking pool, so the runtime's
/// workers stay free while it runs.
pub async fn spawn_quietly<T, F>(work: F) -> Result<T, tokio::task::JoinError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || run_quietly(work)).await
}

fn lower_current_thread() {
    // SAFETY: plain libc call on the current thread with a valid class.
    let result = unsafe { libc::pthread_set_qos_class_self_np(QUIET_QOS, 0) };
    if result != 0 {
        log::warn!("Could not lower post-meeting work to utility QoS (error {result})");
    }
}

/// `QOS_CLASS_UTILITY`'s value, to compare with [`current_qos`].
pub const UTILITY: u32 = libc::qos_class_t::QOS_CLASS_UTILITY as u32;

/// The current thread's QoS class value, for tests and logs.
pub fn current_qos() -> u32 {
    let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
    let mut priority = 0;
    // SAFETY: valid out-pointers, the current thread's handle.
    unsafe { libc::pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut priority) };
    class as u32
}

// ---------------------------------------------------------------------------
// Power source
// ---------------------------------------------------------------------------

/// What the Mac runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerSource {
    /// Plugged in (or a desktop Mac).
    Ac,
    Battery,
    /// IOKit gave no answer: treated as plugged in, so work is never held
    /// back on a guess.
    Unknown,
}

impl PowerSource {
    pub fn on_battery(self) -> bool {
        self == Self::Battery
    }

    /// From IOKit's providing power source type string.
    pub fn from_iokit(kind: Option<&str>) -> Self {
        match kind {
            Some("Battery Power") => Self::Battery,
            Some("AC Power") | Some("UPS Power") => Self::Ac,
            _ => Self::Unknown,
        }
    }
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPSCopyPowerSourcesInfo() -> *const c_void;
    fn IOPSGetProvidingPowerSourceType(snapshot: *const c_void) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
    fn CFStringGetCString(
        string: *const c_void,
        buffer: *mut std::os::raw::c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
}

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// Whether the Mac is on battery right now. Cheap (no I/O beyond IOKit).
pub fn power_source() -> PowerSource {
    // SAFETY: IOKit returns an owned snapshot (released below) and a string
    // borrowed from it; both are checked for null before use.
    unsafe {
        let snapshot = IOPSCopyPowerSourcesInfo();
        if snapshot.is_null() {
            return PowerSource::Unknown;
        }
        let kind = IOPSGetProvidingPowerSourceType(snapshot);
        let mut buffer = [0 as std::os::raw::c_char; 64];
        let text = if !kind.is_null()
            && CFStringGetCString(
                kind,
                buffer.as_mut_ptr(),
                buffer.len() as isize,
                K_CF_STRING_ENCODING_UTF8,
            ) != 0
        {
            std::ffi::CStr::from_ptr(buffer.as_ptr())
                .to_str()
                .ok()
                .map(str::to_string)
        } else {
            None
        };
        CFRelease(snapshot);
        PowerSource::from_iokit(text.as_deref())
    }
}

/// Whether macOS Low Power Mode is on (`NSProcessInfo.isLowPowerModeEnabled`).
pub fn low_power_mode() -> bool {
    use objc::runtime::{Object, BOOL, NO};
    use objc::{class, msg_send, sel, sel_impl};
    // SAFETY: NSProcessInfo and this property exist on macOS 12+, and the
    // app requires macOS 13.
    unsafe {
        let info: *mut Object = msg_send![class!(NSProcessInfo), processInfo];
        if info.is_null() {
            return false;
        }
        let enabled: BOOL = msg_send![info, isLowPowerModeEnabled];
        enabled != NO
    }
}

/// Whether heavy optional work should wait: on battery, or in Low Power
/// Mode (the user asked the Mac to save energy, plugged in or not).
pub fn should_save_power(source: PowerSource, low_power_mode: bool) -> bool {
    source.on_battery() || low_power_mode
}

/// [`should_save_power`] for this Mac right now.
pub fn saving_power_now() -> bool {
    should_save_power(power_source(), low_power_mode())
}

#[cfg(test)]
mod tests {
    #[test]
    fn low_power_mode_holds_work_back_like_the_battery() {
        assert!(!should_save_power(PowerSource::Ac, false));
        assert!(should_save_power(PowerSource::Ac, true));
        assert!(should_save_power(PowerSource::Battery, false));
        assert!(!should_save_power(PowerSource::Unknown, false));
        // The live call works whatever the mode is.
        let _ = low_power_mode();
    }

    use super::*;

    #[test]
    fn quiet_work_runs_at_utility_and_leaves_the_caller_alone() {
        let before = current_qos();
        let inside = run_quietly(current_qos);
        assert_eq!(inside, UTILITY);
        assert_eq!(current_qos(), before);
    }

    #[test]
    fn quiet_work_returns_values_and_propagates_panics() {
        assert_eq!(run_quietly(|| 41 + 1), 42);
        let caught = std::panic::catch_unwind(|| run_quietly(|| panic!("boom")));
        assert!(caught.is_err());
    }

    #[tokio::test]
    async fn spawn_quietly_runs_off_the_runtime_at_utility() {
        let class = spawn_quietly(current_qos).await.unwrap();
        assert_eq!(class, UTILITY);
    }

    #[test]
    fn power_source_parses_iokit_strings_and_never_guesses_battery() {
        assert_eq!(
            PowerSource::from_iokit(Some("Battery Power")),
            PowerSource::Battery
        );
        assert_eq!(PowerSource::from_iokit(Some("AC Power")), PowerSource::Ac);
        assert_eq!(PowerSource::from_iokit(Some("UPS Power")), PowerSource::Ac);
        assert_eq!(PowerSource::from_iokit(None), PowerSource::Unknown);
        assert!(!PowerSource::Unknown.on_battery());
        // The live call works on this Mac, whatever it is plugged into.
        let _ = power_source();
    }
}
