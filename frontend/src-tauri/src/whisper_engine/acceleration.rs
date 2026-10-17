use crate::audio::{GpuType, PerformanceTier};

/// The GPU backend whisper.cpp was compiled with. Minutes ships for Apple
/// Silicon only, where whisper-rs always builds with Metal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhisperCompiledBackend {
    Metal,
}

impl WhisperCompiledBackend {
    pub fn current() -> Self {
        Self::Metal
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metal => "Metal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WhisperContextAcceleration {
    pub compiled_backend: WhisperCompiledBackend,
    pub runtime_detected_gpu: GpuType,
    pub use_gpu: bool,
    pub flash_attn: bool,
    pub gpu_device: i32,
}

impl WhisperContextAcceleration {
    pub fn status_label(self) -> &'static str {
        if self.flash_attn {
            "Metal GPU with Flash Attention (Ultra-Fast)"
        } else {
            "Metal GPU acceleration"
        }
    }
}

pub fn whisper_context_acceleration_for(
    compiled_backend: WhisperCompiledBackend,
    runtime_detected_gpu: GpuType,
    performance_tier: PerformanceTier,
) -> WhisperContextAcceleration {
    let fast_tier = matches!(
        performance_tier,
        PerformanceTier::High | PerformanceTier::Ultra
    );

    WhisperContextAcceleration {
        compiled_backend,
        runtime_detected_gpu,
        use_gpu: true,
        flash_attn: fast_tier,
        gpu_device: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceleration_metal_backend_enables_flash_attention_for_fast_tiers() {
        for tier in [PerformanceTier::High, PerformanceTier::Ultra] {
            let params = whisper_context_acceleration_for(
                WhisperCompiledBackend::Metal,
                GpuType::Metal,
                tier,
            );
            assert!(params.use_gpu);
            assert!(params.flash_attn);
        }
    }

    #[test]
    fn acceleration_metal_backend_skips_flash_attention_for_slow_tiers() {
        for tier in [PerformanceTier::Low, PerformanceTier::Medium] {
            let params = whisper_context_acceleration_for(
                WhisperCompiledBackend::Metal,
                GpuType::None,
                tier,
            );
            assert!(params.use_gpu);
            assert!(!params.flash_attn);
        }
    }
}
