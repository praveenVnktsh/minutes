//! GPU offload sizing: how many layers fit in the detected VRAM.

use std::path::PathBuf;

/// Detect available VRAM in GB
fn detect_vram_gb() -> f32 {
    #[cfg(feature = "metal")]
    {
        // macOS Metal: Query recommended max working set size
        if let Some(vram) = detect_metal_vram() {
            eprintln!("Metal VRAM detected: {:.2} GB", vram);
            return vram;
        }
    }

    eprintln!("VRAM detection not available, using conservative estimate");
    4.0 // Conservative fallback
}

#[cfg(feature = "metal")]
fn detect_metal_vram() -> Option<f32> {
    if let Ok(output) = std::process::Command::new("sysctl")
        .arg("hw.memsize")
        .output()
    {
        if let Ok(stdout) = String::from_utf8(output.stdout) {
            if let Some(bytes_str) = stdout.split(':').nth(1) {
                if let Ok(bytes) = bytes_str.trim().parse::<u64>() {
                    let gb = bytes as f32 / (1024.0 * 1024.0 * 1024.0);
                    // Assume GPU can use ~60% of system memory on Apple Silicon
                    return Some(gb * 0.6);
                }
            }
        }
    }
    None
}

/// Heuristic KV cache size for `context_size` tokens.
/// 7B models (approx > 2.5GB) usually have 4096 hidden dim -> ~256MB per 1k context
/// 1B models (approx < 2.5GB) usually have 2048 hidden dim -> ~128MB per 1k context
fn kv_cache_gb(file_size_gb: f32, context_size: u32) -> f32 {
    let kv_per_1k_gb = if file_size_gb > 2.5 { 0.25 } else { 0.12 };
    (context_size as f32 / 1000.0) * kv_per_1k_gb
}

/// The VRAM left after a 500 MB safety buffer for the OS and display.
fn safe_vram_gb(vram_gb: f32) -> f32 {
    vram_gb - 0.5
}

/// Calculate safe GPU layer count based on VRAM, model file size, and context size
pub(crate) fn layers_that_fit(
    file_size_gb: f32,
    model_layers: u32,
    vram_gb: f32,
    context_size: u32,
) -> u32 {
    if file_size_gb == 0.0 {
        eprintln!("⚠️ Could not determine model file size, using conservative default");
        return 0;
    }

    let total_kv_gb = kv_cache_gb(file_size_gb, context_size);
    let safe_vram = safe_vram_gb(vram_gb);

    // For debugging
    eprintln!("📊 VRAM Analysis:");
    eprintln!("   • Available: {:.2} GB", vram_gb);
    eprintln!("   • Safe Limit: {:.2} GB", safe_vram);
    eprintln!("   • Model Weights: {:.2} GB", file_size_gb);
    eprintln!(
        "   • KV Cache ({} ctx): {:.2} GB",
        context_size, total_kv_gb
    );

    if safe_vram <= 0.0 {
        eprintln!("⚠️ No safe VRAM available, using CPU only");
        return 0;
    }

    // Calculate cost per layer
    let weight_per_layer = file_size_gb / model_layers as f32;
    let kv_per_layer = total_kv_gb / model_layers as f32;
    let total_per_layer = weight_per_layer + kv_per_layer;

    // Calculate how many layers fit
    let safe_layers = (safe_vram / total_per_layer).floor() as u32;
    let layers = safe_layers.min(model_layers);

    eprintln!(
        "   • Cost per layer: {:.2} MB (Weights) + {:.2} MB (KV) = {:.2} MB",
        weight_per_layer * 1024.0,
        kv_per_layer * 1024.0,
        total_per_layer * 1024.0
    );

    if layers < model_layers {
        eprintln!(
            "⚠️ Memory constrained. Offloading {}/{} layers ({:.1}%)",
            layers,
            model_layers,
            (layers as f32 / model_layers as f32) * 100.0
        );
    } else {
        eprintln!("✅ Full offload possible ({} layers)", layers);
    }

    layers
}

/// More layers than any model has: llama.cpp offloads them all.
pub(crate) const ALL_LAYERS: u32 = 999;

/// The context the Metal budget prices the KV cache at: the cached contexts
/// the app asks for are 8k (4k on 8 GB Macs), and on the hybrid and
/// sliding-window models it ships (Qwen 3.5, Gemma 4) even a fresh 32k
/// context's real KV cache is below what the heuristic charges for 8k.
const METAL_KV_BUDGET_CONTEXT: u32 = 8192;

/// Apple Silicon: memory is unified, so every layer goes on Metal when the
/// model fits the safe budget (weights plus the KV estimate, within the
/// working set `detect_vram_gb` allows): a partial offload only moves work
/// to the CPU, where live transcription runs. A model over the budget gets
/// the layer count the sizing logic computes instead of all of them.
pub(crate) fn metal_gpu_layers(
    file_size_gb: f32,
    vram_gb: f32,
    context_size: u32,
    estimated_layers: u32,
) -> u32 {
    let budget_context = context_size.min(METAL_KV_BUDGET_CONTEXT);
    let needed = file_size_gb + kv_cache_gb(file_size_gb, budget_context);
    if file_size_gb > 0.0 && needed <= safe_vram_gb(vram_gb) {
        eprintln!(
            "✅ Metal: all layers ({:.2} GB of a {:.2} GB budget)",
            needed,
            safe_vram_gb(vram_gb)
        );
        return ALL_LAYERS;
    }
    eprintln!(
        "⚠️ Metal: {:.2} GB is over the {:.2} GB budget; sizing the offload",
        needed,
        safe_vram_gb(vram_gb)
    );
    layers_that_fit(file_size_gb, estimated_layers, vram_gb, context_size)
}

/// Get default GPU layer count with smart detection
pub(crate) fn get_default_gpu_layers(model_path: &PathBuf, context_size: u32) -> u32 {
    let vram = detect_vram_gb();
    // TODO: Use actual model metadata instead of heuristics
    // Heuristic: Estimate total layers based on file size
    // 7B models (Q4) are ~4.1GB and have ~32-35 layers
    // 1B models (Q4) are ~1.1GB and have ~20-28 layers
    let file_size_gb = std::fs::metadata(model_path)
        .map(|m| m.len() as f32 / 1024.0 / 1024.0 / 1024.0)
        .unwrap_or(0.0);

    let estimated_layers = if file_size_gb > 2.5 { 33 } else { 28 };

    if cfg!(feature = "metal") {
        return metal_gpu_layers(file_size_gb, vram, context_size, estimated_layers);
    }
    layers_that_fit(file_size_gb, estimated_layers, vram, context_size)
}

// ============================================================================
// Model State Management
// ============================================================================
