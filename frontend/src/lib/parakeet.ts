// Types for Parakeet (NVIDIA NeMo) integration
export interface ParakeetModelInfo {
  name: string;
  path: string;
  size_mb: number;
  /** Exact download size in bytes, straight from the Rust catalogue. This is
   * what the UI must display for download sizes — don't reach for size_mb. */
  size_bytes: number;
  accuracy: ModelAccuracy;
  speed: ProcessingSpeed;
  status: ModelStatus;
  description?: string;
  quantization: QuantizationType;
  /** No longer offered for download; the UI lists it only once installed. */
  legacy: boolean;
}

export type QuantizationType = 'FP32' | 'Int8';
export type ModelAccuracy = 'High' | 'Good' | 'Decent';
export type ProcessingSpeed = 'Slow' | 'Medium' | 'Fast' | 'Very Fast' | 'Ultra Fast';

export type ModelStatus =
  | 'Available'
  | 'Missing'
  | { Downloading: { progress: number } }
  | { Error: string }
  | { Corrupted: { file_size: number; expected_min_size: number } };

export type CancelDownloadOutcome = 'cancelled' | 'pending';
export type ParakeetDownloadEventStatus = 'downloading' | 'completed' | 'cancelled';

export interface ParakeetDownloadProgressEvent {
  modelName: string;
  progress: number;
  downloaded_bytes?: number;
  total_bytes?: number;
  downloaded_mb?: number;
  total_mb?: number;
  speed_mbps?: number;
  status: ParakeetDownloadEventStatus;
  eta_seconds?: number | null;
}

export interface ParakeetEngineState {
  currentModel: string | null;
  availableModels: ParakeetModelInfo[];
  isLoading: boolean;
  error: string | null;
}

// User-friendly model display configuration
export interface ModelDisplayInfo {
  friendlyName: string;
  icon: string;
  tagline: string;
  recommended?: boolean;
  tier: 'fastest' | 'balanced' | 'precise';
}

export const MODEL_DISPLAY_CONFIG: Record<string, ModelDisplayInfo> = {
  'parakeet-tdt-0.6b-v2-int8': {
    friendlyName: 'Lightning',
    icon: '⚡',
    tagline: 'Real time • Most accurate for English',
    recommended: true,
    tier: 'fastest'
  },
  'parakeet-tdt-0.6b-v3-int8': {
    friendlyName: 'Multilingual',
    icon: '🌍',
    tagline: 'Real time • 25 European languages',
    tier: 'balanced'
  }
};

// Helper functions
export function getModelIcon(accuracy: ModelAccuracy): string {
  switch (accuracy) {
    case 'High': return '🔥';
    case 'Good': return '⚡';
    case 'Decent': return '🚀';
    default: return '📊';
  }
}

// Get user-friendly display name for a model
export function getModelDisplayName(modelName: string): string {
  const displayInfo = MODEL_DISPLAY_CONFIG[modelName];
  return displayInfo?.friendlyName || modelName;
}

// Get model display info (icon, tagline, etc.)
export function getModelDisplayInfo(modelName: string): ModelDisplayInfo | null {
  return MODEL_DISPLAY_CONFIG[modelName] || null;
}

export function getStatusColor(status: ModelStatus): string {
  if (status === 'Available') return 'green';
  if (status === 'Missing') return 'gray';
  if (typeof status === 'object' && 'Downloading' in status) return 'blue';
  if (typeof status === 'object' && 'Error' in status) return 'red';
  return 'gray';
}

/** @deprecated Decimal MB/GB formatter kept only for existing callers
 * (ParakeetModelManager, WhisperModelManager). New code should use
 * formatBytes from frontend/src/lib/download-display.ts instead, which is
 * the single binary-unit formatter for sizes shown in the UI. */
export function formatFileSize(sizeMb: number): string {
  if (sizeMb >= 1000) {
    return `${(sizeMb / 1000).toFixed(1)}GB`;
  }
  return `${sizeMb}MB`;
}

// Tauri command wrappers for Parakeet backend
import { invoke } from '@tauri-apps/api/core';

export class ParakeetAPI {
  static async init(): Promise<void> {
    await invoke('parakeet_init');
  }

  static async getAvailableModels(): Promise<ParakeetModelInfo[]> {
    return await invoke('parakeet_get_available_models');
  }

  static async loadModel(modelName: string): Promise<void> {
    await invoke('parakeet_load_model', { modelName });
  }

  static async getCurrentModel(): Promise<string | null> {
    return await invoke('parakeet_get_current_model');
  }

  static async isModelLoaded(): Promise<boolean> {
    return await invoke('parakeet_is_model_loaded');
  }

  static async transcribeAudio(audioData: number[]): Promise<string> {
    return await invoke('parakeet_transcribe_audio', { audioData });
  }

  static async getModelsDirectory(): Promise<string> {
    return await invoke('parakeet_get_models_directory');
  }

  static async downloadModel(modelName: string): Promise<void> {
    await invoke('parakeet_download_model', { modelName });
  }

  static async cancelDownload(modelName: string): Promise<CancelDownloadOutcome> {
    return await invoke('parakeet_cancel_download', { modelName });
  }

  static async deleteCorruptedModel(modelName: string): Promise<string> {
    return await invoke('parakeet_delete_corrupted_model', { modelName });
  }

  static async hasAvailableModels(): Promise<boolean> {
    return await invoke('parakeet_has_available_models');
  }

  static async validateModelReady(): Promise<string> {
    return await invoke('parakeet_validate_model_ready');
  }

  static async openModelsFolder(): Promise<void> {
    await invoke('open_parakeet_models_folder');
  }
}
