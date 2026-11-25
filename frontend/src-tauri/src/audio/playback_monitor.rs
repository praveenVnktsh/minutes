// Audio playback device monitoring for Bluetooth detection
use anyhow::Result;
use serde::Serialize;

use log::debug;

#[derive(Debug, Clone, Serialize)]
pub struct AudioOutputInfo {
    pub device_name: String,
    pub is_bluetooth: bool,
    pub sample_rate: Option<u32>,
    pub device_type: String,
}

/// Get information about the current audio output device
pub async fn get_active_audio_output() -> Result<AudioOutputInfo> {
    get_macos_output().await
}

async fn get_macos_output() -> Result<AudioOutputInfo> {
    use cpal::traits::{DeviceTrait, HostTrait};

    // Get default output device using cpal
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("No default output device found"))?;

    let device_name = device.name().unwrap_or_else(|_| "Unknown".to_string());

    // Get sample rate
    let sample_rate = device
        .default_output_config()
        .ok()
        .map(|config| config.sample_rate().0);

    // Heuristic: Check if device name contains bluetooth-related keywords
    let name_lower = device_name.to_lowercase();
    let is_bluetooth = name_lower.contains("airpods")
        || name_lower.contains("bluetooth")
        || name_lower.contains("wireless")
        || name_lower.contains("wh-")  // Sony WH-* series
        || name_lower.contains("beats")
        || name_lower.contains("bose")
        || name_lower.contains("jabra")
        || name_lower.contains("jbl")
        || name_lower.contains("anker");

    let device_type = if name_lower.contains("speaker") || name_lower.contains("display") {
        "Speaker".to_string()
    } else if name_lower.contains("headphone")
        || name_lower.contains("airpod")
        || name_lower.contains("earbud")
    {
        "Headphones".to_string()
    } else {
        "Unknown".to_string()
    };

    debug!(
        "Active output device: {} (Bluetooth: {}, Type: {}, Rate: {:?} Hz)",
        device_name, is_bluetooth, device_type, sample_rate
    );

    Ok(AudioOutputInfo {
        device_name,
        is_bluetooth,
        sample_rate,
        device_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_output_device() {
        let result = get_active_audio_output().await;
        assert!(result.is_ok(), "Should be able to get output device");

        if let Ok(info) = result {
            println!("Output device: {}", info.device_name);
            println!("Is Bluetooth: {}", info.is_bluetooth);
            println!("Sample rate: {:?}", info.sample_rate);
            println!("Type: {}", info.device_type);
        }
    }
}
