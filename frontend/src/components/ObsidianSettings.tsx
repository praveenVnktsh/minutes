'use client';

import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { FolderOpen, X } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import { SaveFeedback, type SaveFeedbackState } from '@/components/ui/status-feedback';
import { normaliseTags } from '@/lib/meetingExport';
import {
  DEFAULT_OBSIDIAN_CONFIG,
  type ObsidianExportConfig,
  type ObsidianExportState,
} from '@/lib/obsidianExport';
import { badge, panel } from '@/lib/theme-classes';

const inputClass =
  'min-w-0 flex-1 rounded-md border border-hairline bg-surface-raised px-3 py-2 text-sm outline-none focus:border-focus focus:ring-2 focus:ring-focus/30 disabled:opacity-60';

/** The settings as edited: tags stay the raw comma-separated text until saved. */
interface Draft {
  vault_path: string | null;
  folder: string;
  tagsText: string;
  include_transcript: boolean;
}

const toDraft = (config: ObsidianExportConfig): Draft => ({
  vault_path: config.vault_path,
  folder: config.folder,
  tagsText: config.tags.join(', '),
  include_transcript: config.include_transcript,
});

const toConfig = (draft: Draft): ObsidianExportConfig => ({
  vault_path: draft.vault_path,
  folder: draft.folder.trim(),
  tags: normaliseTags(draft.tagsText.split(',')),
  include_transcript: draft.include_transcript,
});

export function ObsidianSettings() {
  const [draft, setDraft] = useState<Draft>(toDraft(DEFAULT_OBSIDIAN_CONFIG));
  const [saved, setSaved] = useState<ObsidianExportConfig>(DEFAULT_OBSIDIAN_CONFIG);
  const [vaultWarning, setVaultWarning] = useState<string | null>(null);
  const [vaultError, setVaultError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveState, setSaveState] = useState<SaveFeedbackState>('saved');

  const applyState = (state: ObsidianExportState) => {
    setDraft(toDraft(state.config));
    setSaved(state.config);
    setVaultWarning(state.vault_warning);
    setVaultError(state.vault_error);
  };

  useEffect(() => {
    invoke<ObsidianExportState>('get_obsidian_export_config')
      .then(applyState)
      .catch((error) => toast.error('Could not load Obsidian settings', { description: String(error) }))
      .finally(() => setLoading(false));
  }, []);

  const config = useMemo(() => toConfig(draft), [draft]);
  const dirty = JSON.stringify(config) !== JSON.stringify(saved);

  const save = async () => {
    setSaving(true);
    setSaveState('saving');
    try {
      applyState(await invoke<ObsidianExportState>('set_obsidian_export_config', { config }));
      setSaveState('saved');
      toast.success('Obsidian settings saved');
    } catch (error) {
      setSaveState('error');
      toast.error('Could not save Obsidian settings', { description: String(error) });
    } finally {
      setSaving(false);
    }
  };

  const chooseVault = async () => {
    try {
      const vault = await invoke<string | null>('pick_obsidian_vault');
      if (!vault) return;
      // A folder chosen inside the old vault means nothing in the new one.
      setDraft((current) => ({ ...current, vault_path: vault, folder: vault === current.vault_path ? current.folder : '' }));
      setVaultWarning(null);
      setVaultError(null);
    } catch (error) {
      toast.error('Could not choose a vault', { description: String(error) });
    }
  };

  // Saving with no vault turns Save to Obsidian off until one is chosen again.
  const clearVault = () => {
    setDraft((current) => ({ ...current, vault_path: null, folder: '' }));
    setVaultWarning(null);
    setVaultError(null);
  };

  const chooseFolder = async () => {
    if (!draft.vault_path) return;
    try {
      const folder = await invoke<string | null>('pick_obsidian_folder', { vaultPath: draft.vault_path });
      if (folder !== null) setDraft((current) => ({ ...current, folder }));
    } catch (error) {
      toast.error('Could not choose a folder', { description: String(error) });
    }
  };

  if (loading) {
    return <div className="mt-6 h-40 animate-pulse rounded-lg bg-surface-2" />;
  }

  return (
    <div className="mt-6 max-w-3xl space-y-6">
      <div>
        <h2 className="text-xl font-semibold">Obsidian</h2>
        <p className="mt-1 text-sm text-ink-muted">
          Save a meeting into your vault from the notes menu, with properties (date, attendees, tags)
          that Dataview and Bases can query. Saving again creates a new version and never changes a
          note you have edited.
        </p>
      </div>

      <div className="space-y-2">
        <label htmlFor="obsidian-vault" className="text-sm font-medium">Vault</label>
        <div className="flex gap-2">
          <input
            id="obsidian-vault"
            readOnly
            value={draft.vault_path ?? ''}
            placeholder="No vault chosen"
            className={inputClass}
          />
          <Button variant="outline" onClick={chooseVault}>
            <FolderOpen className="mr-2 h-4 w-4" /> Choose…
          </Button>
          {draft.vault_path && (
            <Button variant="outline" onClick={clearVault} title="Clear vault" aria-label="Clear vault">
              <X className="h-4 w-4" />
            </Button>
          )}
        </div>
        {vaultError && <div className={`rounded-md p-3 text-sm ${panel.error}`}>{vaultError}</div>}
        {vaultWarning && <div className={`rounded-md p-3 text-sm ${panel.warning}`}>{vaultWarning}</div>}
      </div>

      <div className="space-y-2">
        <label htmlFor="obsidian-folder" className="text-sm font-medium">Folder in the vault</label>
        <div className="flex gap-2">
          <input
            id="obsidian-folder"
            value={draft.folder}
            disabled={!draft.vault_path}
            onChange={(event) => setDraft((current) => ({ ...current, folder: event.target.value }))}
            placeholder="Vault root (e.g. Meetings)"
            className={inputClass}
          />
          <Button variant="outline" onClick={chooseFolder} disabled={!draft.vault_path}>
            <FolderOpen className="mr-2 h-4 w-4" /> Choose…
          </Button>
        </div>
        <p className="text-xs text-ink-muted">Created on first save if it doesn&apos;t exist.</p>
      </div>

      <div className="space-y-2">
        <label htmlFor="obsidian-tags" className="text-sm font-medium">Tags</label>
        <input
          id="obsidian-tags"
          value={draft.tagsText}
          onChange={(event) => setDraft((current) => ({ ...current, tagsText: event.target.value }))}
          placeholder="minutes-app, work"
          className={`w-full ${inputClass}`}
        />
        <div className="flex flex-wrap gap-1.5" aria-label="Tags as saved">
          {config.tags.length === 0
            ? <span className="text-xs text-ink-muted">No tags</span>
            : config.tags.map((tag) => (
              <span key={tag} className={`rounded-full px-2 py-0.5 text-xs ${badge.neutral}`}>#{tag}</span>
            ))}
        </div>
        <p className="text-xs text-ink-muted">
          Comma-separated. Also added to Export. Spaces become dashes; numbers-only tags are dropped.
        </p>
      </div>

      <div className="flex items-center justify-between rounded-lg border bg-surface-raised p-4">
        <div className="pr-4">
          <div className="font-medium">Include transcript</div>
          <div className="text-sm text-ink-muted">
            Applies to Save to Obsidian and Export. Off saves the notes only.
          </div>
        </div>
        <Switch
          aria-label="Include transcript in exports"
          checked={draft.include_transcript}
          onCheckedChange={(include_transcript) => setDraft((current) => ({ ...current, include_transcript }))}
        />
      </div>

      <div className="flex gap-3">
        <Button onClick={save} disabled={saving || !dirty}>
          {saving ? 'Saving…' : 'Save settings'}
        </Button>
      </div>
      <SaveFeedback
        state={saving ? 'saving' : saveState === 'error' ? 'error' : dirty ? 'unsaved' : 'saved'}
        labels={{
          saving: 'Saving Obsidian settings',
          saved: 'Obsidian settings saved',
          error: 'Could not save Obsidian settings',
          unsaved: 'Obsidian changes not saved',
        }}
      />
    </div>
  );
}
