#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use cidre::{core_audio as ca, os};

/// Event types for system audio detection
#[derive(Debug, Clone)]
pub enum SystemAudioEvent {
    SystemAudioStarted(Vec<String>), // List of apps using system audio
    SystemAudioStopped,
}

pub type SystemAudioCallback = std::sync::Arc<dyn Fn(SystemAudioEvent) + Send + Sync + 'static>;

pub fn new_system_audio_callback<F>(f: F) -> SystemAudioCallback
where
    F: Fn(SystemAudioEvent) + Send + Sync + 'static,
{
    std::sync::Arc::new(f)
}

/// Background task manager for system audio detection
#[derive(Default)]
pub struct BackgroundTask {
    handle: Option<tokio::task::JoinHandle<()>>,
    stop_sender: Option<tokio::sync::oneshot::Sender<()>>,
}

impl BackgroundTask {
    pub fn start<F>(&mut self, task: F)
    where
        F: FnOnce(
                std::sync::Arc<std::sync::atomic::AtomicBool>,
                tokio::sync::oneshot::Receiver<()>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + 'static,
    {
        if self.handle.is_some() {
            return; // Already running
        }

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running_clone = running.clone();

        let handle = tokio::spawn(async move {
            task(running_clone, stop_rx).await;
        });

        self.handle = Some(handle);
        self.stop_sender = Some(stop_tx);
    }

    pub fn stop(&mut self) {
        if let Some(sender) = self.stop_sender.take() {
            let _ = sender.send(());
        }

        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

impl Drop for BackgroundTask {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Detects system audio usage on macOS
#[cfg(target_os = "macos")]
pub struct MacOSSystemAudioDetector {
    background: BackgroundTask,
}

#[cfg(target_os = "macos")]
impl Default for MacOSSystemAudioDetector {
    fn default() -> Self {
        Self {
            background: BackgroundTask::default(),
        }
    }
}

#[cfg(target_os = "macos")]
const DEVICE_IS_RUNNING_SOMEWHERE: ca::PropAddr = ca::PropAddr {
    selector: ca::PropSelector::DEVICE_IS_RUNNING_SOMEWHERE,
    scope: ca::PropScope::GLOBAL,
    element: ca::PropElement::MAIN,
};

#[cfg(target_os = "macos")]
struct DetectorState {
    last_state: bool,
    last_change: Instant,
    debounce_duration: Duration,
}

#[cfg(target_os = "macos")]
impl DetectorState {
    fn new() -> Self {
        Self {
            last_state: false,
            last_change: Instant::now(),
            debounce_duration: Duration::from_millis(500),
        }
    }

    fn should_trigger(&mut self, new_state: bool) -> bool {
        let now = Instant::now();

        if new_state == self.last_state {
            return false;
        }
        if now.duration_since(self.last_change) < self.debounce_duration {
            return false;
        }

        self.last_state = new_state;
        self.last_change = now;
        true
    }
}

#[cfg(target_os = "macos")]
impl MacOSSystemAudioDetector {
    pub fn start(&mut self, callback: SystemAudioCallback) {
        self.background.start(|running, mut stop_rx| {
            Box::pin(async move {
                let (tx, mut notify_rx) = tokio::sync::mpsc::channel(1);
                let polling_callback = callback.clone();

                std::thread::spawn(move || {
                    let callback = std::sync::Arc::new(std::sync::Mutex::new(callback));
                    let current_device = std::sync::Arc::new(std::sync::Mutex::new(None::<ca::Device>));
                    let detector_state = std::sync::Arc::new(std::sync::Mutex::new(DetectorState::new()));

                    let callback_for_device = callback.clone();
                    let current_device_for_device = current_device.clone();
                    let detector_state_for_device = detector_state.clone();

                    extern "C-unwind" fn device_listener(
                        _obj_id: ca::Obj,
                        number_addresses: u32,
                        addresses: *const ca::PropAddr,
                        client_data: *mut (),
                    ) -> os::Status {
                        let data = unsafe {
                            &*(client_data as *const (
                                std::sync::Arc<std::sync::Mutex<SystemAudioCallback>>,
                                std::sync::Arc<std::sync::Mutex<Option<ca::Device>>>,
                                std::sync::Arc<std::sync::Mutex<DetectorState>>,
                            ))
                        };
                        let callback = &data.0;
                        let state = &data.2;

                        let addresses = unsafe { std::slice::from_raw_parts(addresses, number_addresses as usize) };

                        for addr in addresses {
                            if addr.selector == ca::PropSelector::DEVICE_IS_RUNNING_SOMEWHERE {
                                if let Ok(device) = ca::System::default_input_device() {
                                    if let Ok(is_running) = device.prop::<u32>(&DEVICE_IS_RUNNING_SOMEWHERE) {
                                        let system_audio_active = is_running != 0;

                                        if let Ok(mut state_guard) = state.lock() {
                                            if state_guard.should_trigger(system_audio_active) {
                                                if system_audio_active {
                                                    let cb = callback.clone();
                                                    std::thread::spawn(move || {
                                                        let apps = list_microphone_using_apps();
                                                        tracing::info!("detect_system_audio_listener: {:?}", apps);

                                                        if let Ok(guard) = cb.lock() {
                                                            let event = SystemAudioEvent::SystemAudioStarted(apps);
                                                            tracing::info!(event = ?event, "detected");
                                                            (*guard)(event);
                                                        }
                                                    });
                                                } else {
                                                    if let Ok(guard) = callback.lock() {
                                                        let event = SystemAudioEvent::SystemAudioStopped;
                                                        tracing::info!(event = ?event, "detected");
                                                        (*guard)(event);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        os::Status::NO_ERR
                    }

                    extern "C-unwind" fn system_listener(
                        _obj_id: ca::Obj,
                        number_addresses: u32,
                        addresses: *const ca::PropAddr,
                        client_data: *mut (),
                    ) -> os::Status {
                        let data = unsafe {
                            &*(client_data as *const (
                                std::sync::Arc<std::sync::Mutex<SystemAudioCallback>>,
                                std::sync::Arc<std::sync::Mutex<Option<ca::Device>>>,
                                std::sync::Arc<std::sync::Mutex<DetectorState>>,
                                *mut (),
                            ))
                        };
                        let current_device = &data.1;
                        let state = &data.2;
                        let device_listener_data = data.3;

                        let addresses = unsafe { std::slice::from_raw_parts(addresses, number_addresses as usize) };

                        for addr in addresses {
                            if addr.selector == ca::PropSelector::HW_DEFAULT_INPUT_DEVICE {
                                if let Ok(mut device_guard) = current_device.lock() {
                                    if let Some(old_device) = device_guard.take() {
                                        let _ = old_device.remove_prop_listener(
                                            &DEVICE_IS_RUNNING_SOMEWHERE,
                                            device_listener,
                                            device_listener_data,
                                        );
                                    }

                                    if let Ok(new_device) = ca::System::default_input_device() {
                                        let system_audio_active = if let Ok(is_running) = new_device.prop::<u32>(&DEVICE_IS_RUNNING_SOMEWHERE) {
                                            is_running != 0
                                        } else {
                                            false
                                        };

                                        if new_device
                                            .add_prop_listener(
                                                &DEVICE_IS_RUNNING_SOMEWHERE,
                                                device_listener,
                                                device_listener_data,
                                            )
                                            .is_ok()
                                        {
                                            *device_guard = Some(new_device);

                                            if let Ok(mut state_guard) = state.lock() {
                                                if state_guard.should_trigger(system_audio_active) {
                                                    if system_audio_active {
                                                        let cb = data.0.clone();
                                                        std::thread::spawn(move || {
                                                            let apps = list_microphone_using_apps();
                                                            tracing::info!("detect_system_listener: {:?}", apps);

                                                            if let Ok(callback_guard) = cb.lock() {
                                                                (*callback_guard)(SystemAudioEvent::SystemAudioStarted(apps));
                                                            }
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        os::Status::NO_ERR
                    }

                    let device_listener_data = Box::new((
                        callback_for_device.clone(),
                        current_device_for_device.clone(),
                        detector_state_for_device.clone(),
                    ));
                    let device_listener_ptr = Box::into_raw(device_listener_data) as *mut ();

                    let system_listener_data = Box::new((
                        callback.clone(),
                        current_device.clone(),
                        detector_state.clone(),
                        device_listener_ptr,
                    ));
                    let system_listener_ptr = Box::into_raw(system_listener_data) as *mut ();

                    if let Err(e) = ca::System::OBJ.add_prop_listener(
                        &ca::PropSelector::HW_DEFAULT_INPUT_DEVICE.global_addr(),
                        system_listener,
                        system_listener_ptr,
                    ) {
                        tracing::error!("adding_system_listener_failed: {:?}", e);
                    } else {
                        tracing::info!("adding_system_listener_success");
                    }

                    if let Ok(device) = ca::System::default_input_device() {
                        let system_audio_active = if let Ok(is_running) = device.prop::<u32>(&DEVICE_IS_RUNNING_SOMEWHERE) {
                            is_running != 0
                        } else {
                            false
                        };

                        if device
                            .add_prop_listener(
                                &DEVICE_IS_RUNNING_SOMEWHERE,
                                device_listener,
                                device_listener_ptr,
                            )
                            .is_ok()
                        {
                            tracing::info!("adding_device_listener_success");

                            if let Ok(mut device_guard) = current_device.lock() {
                                *device_guard = Some(device);
                            }

                            if let Ok(mut state_guard) = detector_state.lock() {
                                state_guard.last_state = system_audio_active;
                            }
                        } else {
                            tracing::error!("adding_device_listener_failed");
                        }
                    } else {
                        tracing::warn!("no_default_output_device_found");
                    }

                    let _ = tx.blocking_send(());

                    loop {
                        std::thread::park();
                    }
                });

                let _ = notify_rx.recv().await;

                let mut last_apps = Vec::new();
                let mut scan_interval = tokio::time::interval(tokio::time::Duration::from_secs(2));

                loop {
                    tokio::select! {
                        _ = &mut stop_rx => {
                            break;
                        }
                        _ = scan_interval.tick() => {
                            if !running.load(std::sync::atomic::Ordering::SeqCst) {
                                break;
                            }

                            let mut apps = list_microphone_using_apps();
                            apps.sort_unstable();
                            apps.dedup();

                            if apps != last_apps {
                                let event = if apps.is_empty() {
                                    SystemAudioEvent::SystemAudioStopped
                                } else {
                                    SystemAudioEvent::SystemAudioStarted(apps.clone())
                                };
                                polling_callback(event);
                                last_apps = apps;
                            }
                        }
                    }
                }
            })
        });
    }

    pub fn stop(&mut self) {
        self.background.stop();
    }
}

#[cfg(target_os = "macos")]
/// Apps currently capturing the microphone. Mic capture is the signal that the
/// user is actually in a call, unlike output audio (music, video, etc.).
fn list_microphone_using_apps() -> Vec<String> {
    match ca::System::processes() {
        Ok(processes) => {
            let mut apps = Vec::new();
            for process in processes {
                if process.is_running_input().unwrap_or(false) {
                    if let Ok(pid) = process.pid() {
                        // Browsers and Electron/WebKit apps capture the mic from a
                        // helper process that isn't a running app itself, so name
                        // the app responsible for it (Chrome helper → Google Chrome).
                        let pid = responsible_pid(pid);
                        if let Some(running_app) = cidre::ns::RunningApp::with_pid(pid) {
                            let name = running_app
                                .localized_name()
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| format!("Process {}", pid));
                            if !apps.contains(&name) {
                                apps.push(name);
                            }
                        }
                    }
                }
            }

            // Browsers hide which tab is open, so probe for a Meet URL among the
            // browsers that are actually capturing the mic.
            if browser_capturing_mic_has_google_meet(&apps) {
                apps.push("Google Meet".to_string());
            }
            apps
        }
        Err(_) => Vec::new(),
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    // libquarantine (part of libSystem): the pid macOS holds responsible for
    // `pid`, e.g. the browser that owns a helper or WebKit GPU process.
    fn responsibility_get_pid_responsible_for_pid(pid: i32) -> i32;
}

#[cfg(target_os = "macos")]
fn responsible_pid(pid: i32) -> i32 {
    match unsafe { responsibility_get_pid_responsible_for_pid(pid) } {
        responsible if responsible > 0 => responsible,
        _ => pid,
    }
}

#[cfg(target_os = "macos")]
fn browser_capturing_mic_has_google_meet(capturing: &[String]) -> bool {
    const BROWSERS: [&str; 3] = ["Google Chrome", "Microsoft Edge", "Safari"];

    BROWSERS.iter().any(|application_name| {
        capturing
            .iter()
            .any(|app| app.eq_ignore_ascii_case(application_name))
            && browser_has_google_meet_tab(application_name)
    })
}

#[cfg(target_os = "macos")]
fn browser_has_google_meet_tab(application_name: &str) -> bool {
    let script = format!(
        "tell application \"{}\" to get URL of every tab of every window",
        application_name
    );
    let mut child = match std::process::Command::new("osascript")
        .args(["-e", &script])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        _ => return false,
    };

    let deadline = Instant::now() + Duration::from_millis(1500);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = match child.wait_with_output() {
                    Ok(output) if status.success() => output,
                    _ => return false,
                };
                return google_meet_url_is_open(&String::from_utf8_lossy(&output.stdout));
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn google_meet_url_is_open(browser_urls: &str) -> bool {
    browser_urls.split(',').any(|url| {
        let Some(path) = url.trim().strip_prefix("https://meet.google.com/") else {
            return false;
        };
        let meeting_code = path.split(['?', '#']).next().unwrap_or_default();
        let parts: Vec<_> = meeting_code.split('-').collect();
        parts.len() == 3
            && [3, 4, 3].iter().zip(parts).all(|(length, part)| {
                part.len() == *length && part.chars().all(|c| c.is_ascii_alphabetic())
            })
    })
}

// Stub implementation for non-macOS platforms
#[cfg(not(target_os = "macos"))]
pub struct MacOSSystemAudioDetector;

#[cfg(not(target_os = "macos"))]
impl Default for MacOSSystemAudioDetector {
    fn default() -> Self {
        Self
    }
}

#[cfg(not(target_os = "macos"))]
impl MacOSSystemAudioDetector {
    pub fn start(&mut self, _callback: SystemAudioCallback) {
        tracing::warn!("System audio detection is only supported on macOS");
    }

    pub fn stop(&mut self) {}
}

/// Public interface for system audio detection
#[derive(Default)]
pub struct SystemAudioDetector {
    inner: MacOSSystemAudioDetector,
}

impl SystemAudioDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(&mut self, callback: SystemAudioCallback) {
        self.inner.start(callback);
    }

    pub fn stop(&mut self) {
        self.inner.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn recognizes_google_meet_call_urls_only() {
        assert!(google_meet_url_is_open(
            "https://example.com, https://meet.google.com/abc-defg-hij"
        ));
        assert!(google_meet_url_is_open(
            "https://meet.google.com/abc-defg-hij?authuser=0"
        ));
        assert!(!google_meet_url_is_open("https://meet.google.com/"));
        assert!(!google_meet_url_is_open("https://meet.google.com/landing"));
        assert!(!google_meet_url_is_open("https://youtube.com/watch?v=123"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn helper_processes_resolve_to_their_owning_app() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .spawn()
            .expect("spawn child");
        let child_pid = child.id() as i32;

        // A spawned child shares its parent's responsible app, the same way a
        // browser's audio helper resolves to the browser.
        assert_eq!(
            responsible_pid(child_pid),
            responsible_pid(std::process::id() as i32)
        );
        assert_ne!(responsible_pid(child_pid), child_pid);

        let _ = child.kill();
        let _ = child.wait();
    }

    #[tokio::test]
    #[ignore] // Only run manually as it requires audio hardware
    async fn test_system_audio_detector() {
        let mut detector = SystemAudioDetector::new();
        detector.start(new_system_audio_callback(|event| {
            println!("System audio event: {:?}", event);
        }));

        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
        detector.stop();
    }
}
