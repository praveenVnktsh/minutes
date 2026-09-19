import React, { useState, useEffect, useRef } from 'react';
import { Switch } from '@/components/ui/switch';
import { FolderOpen } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { DeviceSelection, SelectedDevices } from '@/components/DeviceSelection';
import Analytics from '@/lib/analytics';
import { toast } from 'sonner';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { useConfig } from '@/contexts/ConfigContext';
import { SaveFeedback, type SaveFeedbackState } from '@/components/ui/status-feedback';

export interface RecordingPreferences {
  save_folder: string;
  auto_save: boolean;
  file_format: string;
  automatic_record_prompt: boolean;
  min_meeting_duration_seconds: number;
  preferred_mic_device: string | null;
  preferred_system_device: string | null;
}

interface RecordingSettingsProps {
  onSave?: (preferences: RecordingPreferences) => void;
}

export function RecordingSettings({ onSave }: RecordingSettingsProps) {
  const [preferences, setPreferences] = useState<RecordingPreferences>({
    save_folder: '',
    auto_save: true,
    file_format: 'mp4',
    automatic_record_prompt: true,
    min_meeting_duration_seconds: 10,
    preferred_mic_device: null,
    preferred_system_device: null
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [showRecordingNotification, setShowRecordingNotification] = useState(true);
  const [preferenceSaveState, setPreferenceSaveState] = useState<SaveFeedbackState | null>(null);
  const [preferenceMessage, setPreferenceMessage] = useState('Recording preference');
  const [notificationSaveState, setNotificationSaveState] = useState<SaveFeedbackState | null>(null);
  const [notificationFailureMessage, setNotificationFailureMessage] = useState('Could not save participant reminder; previous setting restored');
  const notificationRevision = useRef(0);
  const notificationSaving = useRef(false);
  const { isRecording } = useRecordingState();
  const { setSelectedDevices } = useConfig();

  // Load recording preferences on component mount
  useEffect(() => {
    const loadPreferences = async () => {
      try {
        const prefs = await invoke<RecordingPreferences>('get_recording_preferences');
        setPreferences(prefs);
      } catch (error) {
        console.error('Failed to load recording preferences:', error);
        // If loading fails, get default folder path
        try {
          const defaultPath = await invoke<string>('get_default_recordings_folder_path');
          setPreferences(prev => ({ ...prev, save_folder: defaultPath }));
        } catch (defaultError) {
          console.error('Failed to get default folder path:', defaultError);
        }
      } finally {
        setLoading(false);
      }
    };

    loadPreferences();
  }, []);

  // Load recording notification preference
  useEffect(() => {
    const loadNotificationPref = async () => {
      const revision = notificationRevision.current;
      try {
        const { Store } = await import('@tauri-apps/plugin-store');
        const store = await Store.load('preferences.json');
        const show = await store.get<boolean>('show_recording_notification') ?? true;
        if (revision === notificationRevision.current) setShowRecordingNotification(show);
      } catch (error) {
        console.error('Failed to load notification preference:', error);
      }
    };
    loadNotificationPref();
  }, []);

  const handleAutoSaveToggle = async (enabled: boolean) => {
    const previous = preferences;
    const newPreferences = { ...preferences, auto_save: enabled };
    setPreferences(newPreferences);
    if (!await savePreferences(newPreferences, 'Audio recording preference')) {
      setPreferences(previous);
      return;
    }

    // Track auto-save setting change
    await Analytics.track('auto_save_recording_toggled', {
      enabled: enabled.toString()
    });
  };

  const handleAutomaticPromptToggle = async (enabled: boolean) => {
    const previous = preferences;
    const newPreferences = { ...preferences, automatic_record_prompt: enabled };
    setPreferences(newPreferences);
    if (!await savePreferences(newPreferences, 'Meeting detection preference')) {
      setPreferences(previous);
      return;
    }

    await Analytics.track('automatic_record_prompt_toggled', {
      enabled: enabled.toString()
    });
  };

  const handleMinDurationChange = async (value: string) => {
    const previous = preferences;
    const seconds = Math.max(0, Math.min(3600, Math.round(Number(value) || 0)));
    const newPreferences = { ...preferences, min_meeting_duration_seconds: seconds };
    setPreferences(newPreferences);
    if (!await savePreferences(newPreferences, 'Short recording policy')) setPreferences(previous);
  };

  const handleDeviceChange = async (devices: SelectedDevices) => {
    const previous = preferences;
    const newPreferences = {
      ...preferences,
      preferred_mic_device: devices.micDevice,
      preferred_system_device: devices.systemDevice
    };
    setPreferences(newPreferences);
    // Sync the in-memory selection ConfigContext exposes to the start path.
    // Without this, the picker only writes to disk (loaded into context once at
    // app mount), so a newly-chosen mic isn't honored until the next launch —
    // start keeps sending the stale launch-time device.
    setSelectedDevices(devices);
    if (!await savePreferences(newPreferences, 'Default audio devices')) {
      setPreferences(previous);
      setSelectedDevices({
        micDevice: previous.preferred_mic_device,
        systemDevice: previous.preferred_system_device,
      });
      return;
    }

    // Track default device preference changes
    // Note: Individual device selection analytics are tracked in DeviceSelection component
    await Analytics.track('default_devices_changed', {
      has_preferred_microphone: (!!devices.micDevice).toString(),
      has_preferred_system_audio: (!!devices.systemDevice).toString()
    });
  };

  const handleOpenFolder = async () => {
    try {
      await invoke('open_recordings_folder');
    } catch (error) {
      console.error('Failed to open recordings folder:', error);
      toast.error('Could not open recordings folder', { description: String(error) });
    }
  };

  const handleNotificationToggle = async (enabled: boolean) => {
    if (notificationSaving.current) return;
    notificationSaving.current = true;
    notificationRevision.current += 1;
    const previous = showRecordingNotification;
    setShowRecordingNotification(enabled);
    setNotificationSaveState('saving');
    setNotificationFailureMessage('Could not save participant reminder; previous setting restored');
    let store: { set: (key: string, value: unknown) => Promise<void>; save: () => Promise<void> } | null = null;
    try {
      const { Store } = await import('@tauri-apps/plugin-store');
      store = await Store.load('preferences.json');
      await store.set('show_recording_notification', enabled);
      await store.save();
      setNotificationSaveState('saved');
      await Analytics.track('recording_notification_preference_changed', {
        enabled: enabled.toString()
      });
    } catch (error) {
      console.error('Failed to save notification preference:', error);
      setShowRecordingNotification(previous);
      if (store) {
        try {
          await store.set('show_recording_notification', previous);
          await store.save();
        } catch (restoreError) {
          console.error('Failed to restore notification preference:', restoreError);
          setNotificationFailureMessage('Could not save or restore the participant reminder; reopen settings to verify it');
        }
      }
      setNotificationSaveState('error');
    } finally {
      notificationSaving.current = false;
    }
  };

  const savePreferences = async (prefs: RecordingPreferences, operation: string): Promise<boolean> => {
    setSaving(true);
    setPreferenceMessage(operation);
    setPreferenceSaveState('saving');
    try {
      await invoke('set_recording_preferences', { preferences: prefs });
      onSave?.(prefs);
      window.dispatchEvent(new CustomEvent('automaticRecordPromptChanged', {
        detail: prefs.automatic_record_prompt
      }));

      setPreferenceSaveState('saved');
      return true;
    } catch (error) {
      console.error('Failed to save recording preferences:', error);
      setPreferenceSaveState('error');
      return false;
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return (
      <div className="animate-pulse">
        <div className="h-4 bg-surface-2 rounded w-1/4 mb-4"></div>
        <div className="h-8 bg-surface-2 rounded mb-4"></div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-lg font-semibold mb-4">Recording Settings</h3>
        <p className="text-sm text-ink-muted mb-6">
          Configure how your audio recordings are saved during meetings.
        </p>
        {preferenceSaveState && (
          <SaveFeedback
            state={preferenceSaveState}
            labels={{
              saving: `Saving ${preferenceMessage.toLowerCase()}`,
              saved: `${preferenceMessage} saved`,
              error: `Could not save ${preferenceMessage.toLowerCase()}; previous setting restored`,
            }}
          />
        )}
      </div>

      {/* Auto Save Toggle */}
      <div className="flex items-center justify-between p-4 border rounded-lg">
        <div className="flex-1">
          <div className="font-medium">Save Audio Recordings</div>
          <div className="text-sm text-ink-muted">
            Automatically save audio files when recording stops
          </div>
        </div>
        <Switch
          aria-label="Save audio recordings"
          checked={preferences.auto_save}
          onCheckedChange={handleAutoSaveToggle}
          disabled={saving}
        />
      </div>

      <div className="flex items-center justify-between p-4 border rounded-lg">
        <div className="flex-1 pr-4">
          <div className="font-medium">Meeting Detection Prompt</div>
          <div className="text-sm text-ink-muted">
            Ask before recording when sustained audio is detected from a supported meeting app
          </div>
        </div>
        <Switch
          aria-label="Show meeting detection prompt"
          checked={preferences.automatic_record_prompt}
          onCheckedChange={handleAutomaticPromptToggle}
          disabled={saving}
        />
      </div>

      <div className="flex items-center justify-between p-4 border rounded-lg">
        <div className="flex-1 pr-4">
          <div className="font-medium">Discard short recordings</div>
          <div className="text-sm text-ink-muted">
            Delete meetings shorter than this that have no transcript or notes (0 disables).
          </div>
        </div>
        <div className="flex items-center gap-2">
          <input
            type="number"
            min={0}
            max={3600}
            value={preferences.min_meeting_duration_seconds}
            onChange={(event) => void handleMinDurationChange(event.target.value)}
            aria-label="Minimum meeting duration in seconds"
            disabled={saving}
            className="h-9 w-20 rounded-md border border-hairline bg-surface-2 px-2 text-sm text-ink"
          />
          <span className="text-sm text-ink-muted">seconds</span>
        </div>
      </div>

      {/* Folder Location - Only shown when auto_save is enabled */}
      {preferences.auto_save && (
        <div className="space-y-4">
          <div className="p-4 border rounded-lg bg-surface-2">
            <div className="font-medium mb-2">Save Location</div>
            <div className="text-sm text-ink-muted mb-3 break-all">
              {preferences.save_folder || 'Default folder'}
            </div>
            <button
              onClick={handleOpenFolder}
              className="flex items-center gap-2 px-3 py-2 text-sm border border-hairline rounded-md hover:bg-surface-2 transition-colors"
            >
              <FolderOpen className="w-4 h-4" />
              Open Folder
            </button>
          </div>

          <div className="rounded-lg border border-info bg-info-subtle p-4">
            <div className="text-sm text-info">
              <strong>File Format:</strong> {preferences.file_format.toUpperCase()} files
            </div>
            <div className="mt-1 text-xs text-info">
              Recordings are saved with timestamp: recording_YYYYMMDD_HHMMSS.{preferences.file_format}
            </div>
          </div>
        </div>
      )}

      {/* Info when auto_save is disabled */}
      {!preferences.auto_save && (
        <div className="rounded-lg border border-warning bg-warning-subtle p-4">
          <div className="text-sm text-warning">
            Audio recording is disabled. Enable Save Audio Recordings to automatically save your meeting audio.
          </div>
        </div>
      )}

      {/* Recording Notification Toggle */}
      <div className="flex items-center justify-between p-4 border rounded-lg">
        <div className="flex-1">
          <div className="font-medium">Recording Start Notification</div>
          <div className="text-sm text-ink-muted">
            Show reminder to inform participants when recording starts
          </div>
        </div>
        <Switch
          aria-label="Remind me to inform participants when recording starts"
          checked={showRecordingNotification}
          onCheckedChange={handleNotificationToggle}
          disabled={notificationSaveState === 'saving'}
        />
      </div>
      {notificationSaveState && (
        <SaveFeedback
          state={notificationSaveState}
          labels={{
            saving: 'Saving participant reminder',
            saved: 'Participant reminder saved',
            error: notificationFailureMessage,
          }}
        />
      )}

      {/* Device Preferences */}
      <div className="space-y-4">
        <div className="border-t pt-6">
          <h4 className="text-base font-medium text-ink mb-4">Default Audio Devices</h4>
          <p className="text-sm text-ink-muted mb-4">
            Set your preferred microphone and system audio devices for recording. These will be automatically selected when starting new recordings.
          </p>

          {isRecording && (
            <p
              role="status"
              aria-live="polite"
              className="mb-4 rounded-md border border-warning bg-warning-subtle p-2 text-sm text-warning"
            >
              Device selection is locked while a recording is in progress. Connecting a new device mid-recording will not switch to it. Stop the current meeting to change devices.
            </p>
          )}

          <div className="border rounded-lg p-4 bg-surface-2">
            <DeviceSelection
              selectedDevices={{
                micDevice: preferences.preferred_mic_device,
                systemDevice: preferences.preferred_system_device
              }}
              onDeviceChange={handleDeviceChange}
              disabled={saving || isRecording}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
