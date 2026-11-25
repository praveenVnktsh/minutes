use std::process::Command;

/// Name of the running executable, used to filter `log stream` output.
///
/// The dev binary is `meetily` while the bundled app uses the product name
/// (`minutes`), so resolve it at runtime instead of hardcoding either.
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

#[tauri::command]
pub fn hide_console() -> Result<String, String> {
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

#[tauri::command]
pub fn toggle_console() -> Result<String, String> {
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
