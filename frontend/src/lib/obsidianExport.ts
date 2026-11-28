/** Mirrors `ObsidianExportConfig` in `src-tauri/src/obsidian.rs`. */
export interface ObsidianExportConfig {
  vault_path: string | null;
  /** Folder inside the vault, `/`-separated; empty for the vault's root. */
  folder: string;
  tags: string[];
  include_transcript: boolean;
}

export interface ObsidianExportState {
  config: ObsidianExportConfig;
  /** The vault has no `.obsidian/` folder; saving still works. */
  vault_warning: string | null;
  /** The stored vault can't be used; saving fails until it's chosen again. */
  vault_error: string | null;
}

export interface SavedObsidianNote {
  path: string;
  relative_path: string;
  vault_name: string;
  /** 1 for the plain file name, 2 for ` v2`, … */
  version: number;
}

export const DEFAULT_OBSIDIAN_CONFIG: ObsidianExportConfig = {
  vault_path: null,
  folder: '',
  tags: ['minutes-app'],
  include_transcript: true,
};
