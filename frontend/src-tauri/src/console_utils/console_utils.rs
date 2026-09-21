#[cfg(target_os = "windows")]
use env_logger;
#[cfg(target_os = "macos")]
use std::process::Command;
#[cfg(target_os = "windows")]
use std::ptr;

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
extern "system" {
    fn AllocConsole() -> i32;
    #[allow(dead_code)]
    fn FreeConsole() -> i32;
    fn GetConsoleWindow() -> *mut std::ffi::c_void;
    fn ShowWindow(hwnd: *mut std::ffi::c_void, n_cmd_show: i32) -> i32;
}

#[cfg(target_os = "windows")]
const SW_HIDE: i32 = 0;
#[cfg(target_os = "windows")]
const SW_SHOW: i32 = 5;

/// Name of the running executable, used to filter `log stream` output.
///
/// The dev binary is `meetily` while the bundled app uses the product name
/// (`minutes`), so resolve it at runtime instead of hardcoding either.
#[cfg(target_os = "macos")]
fn current_process_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "minutes".to_string())
}

#[tauri::command]
pub fn show_console() -> Result<String, String> {
    #[cfg(target_os = "windows")]
    unsafe {
        let console_window = GetConsoleWindow();
        if console_window == ptr::null_mut() {
            // If no console exists, allocate one
            if AllocConsole() == 0 {
                return Err("Failed to allocate console".to_string());
            }
            // Reinitialize stdout, stdin, stderr for the new console
            std::env::set_var("RUST_LOG", "info");
            env_logger::init();
        } else {
            // Show existing console window
            ShowWindow(console_window, SW_SHOW);
        }
        Ok("Console shown".to_string())
    }

    #[cfg(target_os = "macos")]
    {
        // On macOS, we'll open Terminal.app with our app's logs
        let process_name = current_process_name();
        let script = format!(
            r#"
                tell application "Terminal"
                    activate
                    do script "log stream --process {} --level info --style compact"
                end tell
            "#,
            process_name
        );

        match Command::new("osascript").arg("-e").arg(script).spawn() {
            Ok(_) => Ok("Console opened in Terminal".to_string()),
            Err(e) => Err(format!("Failed to open console: {}", e)),
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Ok("Console control is only available on Windows and macOS".to_string())
    }
}

#[tauri::command]
pub fn hide_console() -> Result<String, String> {
    #[cfg(target_os = "windows")]
    unsafe {
        let console_window = GetConsoleWindow();
        if console_window != ptr::null_mut() {
            ShowWindow(console_window, SW_HIDE);
            Ok("Console hidden".to_string())
        } else {
            Err("No console window found".to_string())
        }
    }

    #[cfg(target_os = "macos")]
    {
        // On macOS, we'll close the Terminal window that's showing our logs
        let process_name = current_process_name();
        let script = format!(
            r#"
                tell application "Terminal"
                    set windowList to windows
                    repeat with aWindow in windowList
                        if contents of selected tab of aWindow contains "log stream --process {}" then
                            close aWindow
                        end if
                    end repeat
                end tell
            "#,
            process_name
        );

        match Command::new("osascript").arg("-e").arg(script).spawn() {
            Ok(_) => Ok("Console closed".to_string()),
            Err(e) => Err(format!("Failed to close console: {}", e)),
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Ok("Console control is only available on Windows and macOS".to_string())
    }
}

#[tauri::command]
pub fn toggle_console() -> Result<String, String> {
    #[cfg(target_os = "windows")]
    unsafe {
        let console_window = GetConsoleWindow();
        if console_window == ptr::null_mut() {
            show_console()
        } else {
            // Check if window is visible (this is a simplified approach)
            // In a real implementation, you might want to use GetWindowLong to check visibility
            hide_console()
        }
    }

    #[cfg(target_os = "macos")]
    {
        // On macOS, check if Terminal is running with our log stream
        let process_name = current_process_name();
        let script = format!(
            r#"
                tell application "Terminal"
                    set windowList to windows
                    repeat with aWindow in windowList
                        if contents of selected tab of aWindow contains "log stream --process {}" then
                            return "found"
                        end if
                    end repeat
                    return "not found"
                end tell
            "#,
            process_name
        );

        let check_result = Command::new("osascript").arg("-e").arg(script).output();

        match check_result {
            Ok(output) => {
                let output_str = String::from_utf8_lossy(&output.stdout);
                if output_str.trim() == "found" {
                    hide_console()
                } else {
                    show_console()
                }
            }
            Err(_) => show_console(),
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Ok("Console control is only available on Windows and macOS".to_string())
    }
}
