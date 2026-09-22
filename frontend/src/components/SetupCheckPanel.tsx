import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { Cpu, Loader2, Mic, RefreshCw, Speaker, Square } from 'lucide-react';
import { motion, AnimatePresence } from 'framer-motion';
import { Button } from '@/components/ui/button';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { AudioLevelMeter } from '@/components/AudioLevelMeter';
import { cn } from '@/lib/utils';
import type { RecordingPreferences } from '@/components/RecordingSettings';
import {
  applySetupCheckFix,
  cancelSetupCheck,
  describeChannelOutcome,
  describeModelOutcome,
  onSetupCheckLevel,
  startSetupCheck,
  summariseSetupCheck,
  SETUP_CHECK_DURATION_SECONDS,
  type ChannelOutcome,
  type ChannelReport,
  type ModelCheckOutcome,
  type SetupCheckChannel,
  type SetupCheckDescription,
  type SetupCheckFix,
  type SetupCheckFixKind,
  type SetupCheckLevel,
  type SetupCheckRecord,
  type SetupCheckResult,
} from '@/lib/micCheck';

/**
 * The setup check, as a component, because two surfaces run it: the last step of
 * onboarding and the settings pane that re-offers it afterwards. Neither of them
 * owns the behaviour — this does — so a fix to the wording or the device
 * handling lands in both places at once.
 *
 * The check reports the model, the microphone and the system audio as three
 * separate results and deliberately never rolls them into one verdict. A Mac
 * with no meeting running is supposed to finish with a working microphone and a
 * silent system-audio channel; that pair has to read as one good result and one
 * neutral one, because a red banner over the whole thing would teach the user to
 * distrust a check that was telling them the truth.
 */

export interface SetupCheckPanelProps {
  /**
   * Fired every time a run finishes — cancelled runs included — with the record
   * already summarised, so both surfaces persist the same thing without
   * re-deriving it from the raw result.
   */
  onOutcome?: (record: SetupCheckRecord, result: SetupCheckResult) => void;
  /**
   * Where "download the model again" goes. Onboarding routes it back to the
   * download step; settings has nowhere to route to and omits this, and the
   * panel then writes the fix out as instructions instead.
   */
  onRetryDownload?: () => void;
  /**
   * Fired whenever the user picks a device here, with BOTH channels' current
   * selections — the in-memory selection it feeds replaces the pair, so sending
   * only the one that changed would clear the other.
   *
   * Saving a preference to disk is not enough on its own: ConfigContext reads
   * those preferences once, at app mount, and the recording start path sends
   * what that context holds. Without this, a device chosen here is validated by
   * the check and then not used by the very next meeting, which is the one
   * outcome this whole step exists to rule out.
   *
   * Onboarding omits it and must: the flow renders outside ConfigProvider, so
   * there is no in-memory selection to correct there, and finishing setup
   * reloads the window, which remounts the context from disk.
   */
  onDevicesChanged?: (devices: { micDevice: string | null; systemDevice: string | null }) => void;
  autoStart?: boolean;
  className?: string;
}

interface AudioDevice {
  name: string;
  device_type: 'Input' | 'Output';
}

/**
 * 'preparing' is everything before the first level arrives: the model is being
 * loaded and nothing is being captured yet. The Rust command does the load and
 * both captures in one call, so the level events are what tells us the capture
 * window has actually opened.
 */
type CheckPhase = 'idle' | 'preparing' | 'capturing' | 'transcribing' | 'done';

/** The form the backend parses and the saved preference stores. */
const micDeviceValue = (device: AudioDevice) => `${device.name} (input)`;
const systemDeviceValue = (device: AudioDevice) => `${device.name} (output)`;

const DEFAULT_MIC_LABEL = 'Default microphone';
const DEFAULT_SYSTEM_LABEL = 'Default system audio';

/** Meters and messages want the device, not the suffix the backend parses. */
function displayDeviceName(value: string | null, fallback: string): string {
  if (!value) return fallback;
  return value.replace(/\s*\((?:input|output)\)$/, '');
}

/**
 * How strongly a card reads. Three tones rather than pass/fail, because the
 * difference between "nothing was playing through your speakers" and "the OS is
 * withholding your microphone" is the whole point of reporting the channels
 * separately: the first is the ordinary result on a quiet machine and must stay
 * neutral, the second is something the user has to go and switch on.
 */
type CardTone = 'good' | 'neutral' | 'problem';

const MODEL_TONES: Record<ModelCheckOutcome, CardTone> = {
  loaded: 'good',
  unavailable: 'problem',
  failed: 'problem',
  command_failed: 'problem',
};

const MICROPHONE_TONES: Record<ChannelOutcome, CardTone> = {
  transcribed: 'good',
  no_speech_detected: 'neutral',
  // A microphone that sent silence is muted or is not the one being spoken into,
  // which is exactly the failure this check exists to catch before a meeting.
  no_audio_detected: 'problem',
  permission_denied: 'problem',
  device_unavailable: 'problem',
  transcription_failed: 'problem',
  cancelled: 'neutral',
  unsupported: 'neutral',
  not_run: 'neutral',
  command_failed: 'problem',
};

const SYSTEM_AUDIO_TONES: Record<ChannelOutcome, CardTone> = {
  transcribed: 'good',
  no_speech_detected: 'neutral',
  // Not a problem. A computer with nothing playing sounds like this, and the
  // check runs before the user has a meeting open, so this is the expected
  // result and must never be dressed up as a failure.
  no_audio_detected: 'neutral',
  permission_denied: 'problem',
  device_unavailable: 'problem',
  transcription_failed: 'problem',
  cancelled: 'neutral',
  unsupported: 'neutral',
  not_run: 'neutral',
  command_failed: 'problem',
};

const TONE_CLASSES: Record<CardTone, string> = {
  good: 'bg-success-subtle border-success',
  neutral: 'bg-surface-raised border-hairline',
  problem: 'bg-error-subtle border-error',
};

/**
 * What to tell the user when we could not carry the fix out for them. The two
 * settings panes only open on macOS, and a settings surface has no download step
 * to send anyone to, so every fix button needs a written fallback.
 */
const MANUAL_FIX_INSTRUCTIONS: Partial<Record<SetupCheckFixKind, string>> = {
  'open-mic-settings':
    'Minutes could not open the settings for you. Open them yourself: System Settings › Privacy & Security › Microphone on macOS, or Settings › Privacy & security › Microphone on Windows. Switch Minutes on, then run the check again.',
  'open-screen-recording-settings':
    'Minutes could not open the settings for you. Open them yourself: System Settings › Privacy & Security › Screen Recording on macOS. Switch Minutes on, quit and reopen Minutes, then run the check again.',
  'retry-download':
    'Open Settings › Transcription and download the transcription model again, then run this check once it has finished.',
};

export function SetupCheckPanel({
  onOutcome,
  onRetryDownload,
  onDevicesChanged,
  autoStart = false,
  className,
}: SetupCheckPanelProps) {
  const [micDevices, setMicDevices] = useState<AudioDevice[]>([]);
  const [systemDevices, setSystemDevices] = useState<AudioDevice[]>([]);
  const [deviceError, setDeviceError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);

  // null is the system default, matching what the backend does with no device name.
  const [selectedMic, setSelectedMic] = useState<string | null>(null);
  const [selectedSystem, setSelectedSystem] = useState<string | null>(null);

  const [phase, setPhase] = useState<CheckPhase>('idle');
  const [levels, setLevels] = useState<Partial<Record<SetupCheckChannel, SetupCheckLevel>>>({});
  const [result, setResult] = useState<SetupCheckResult | null>(null);
  // Fixes we could not carry out, so the card can print the instructions instead.
  const [manualFixes, setManualFixes] = useState<SetupCheckFixKind[]>([]);

  const unlistenLevelRef = useRef<UnlistenFn | null>(null);
  const captureTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const micTriggerRef = useRef<HTMLButtonElement>(null);
  const systemTriggerRef = useRef<HTMLButtonElement>(null);
  const mountedRef = useRef(true);
  // Held in a ref so a caller that re-renders with a fresh closure each time
  // cannot strand a run that is already in flight with the previous one.
  const onOutcomeRef = useRef(onOutcome);
  onOutcomeRef.current = onOutcome;

  const isRunning = phase === 'preparing' || phase === 'capturing' || phase === 'transcribing';

  const loadDevices = useCallback(async () => {
    const all = await invoke<AudioDevice[]>('get_audio_devices');
    const inputs = all.filter((device) => device.device_type === 'Input');
    const outputs = all.filter((device) => device.device_type === 'Output');
    setMicDevices(inputs);
    setSystemDevices(outputs);
    setDeviceError(null);
    return { inputs, outputs };
  }, []);

  const stopLevelUpdates = useCallback(() => {
    if (captureTimerRef.current) {
      clearTimeout(captureTimerRef.current);
      captureTimerRef.current = null;
    }
    if (unlistenLevelRef.current) {
      unlistenLevelRef.current();
      unlistenLevelRef.current = null;
    }
  }, []);

  // Leaving the screen mid-check would otherwise hold both devices open.
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      stopLevelUpdates();
      cancelSetupCheck();
    };
  }, [stopLevelUpdates]);

  /**
   * Writes one preference back without disturbing the others, so choosing a
   * microphone here does not silently reset the save folder or the format. A
   * preference that will not save is never worth blocking anyone over: the check
   * itself still runs against the device the user picked.
   */
  const savePreferredDevice = useCallback(
    async (field: 'preferred_mic_device' | 'preferred_system_device', device: string | null) => {
      try {
        const preferences = await invoke<RecordingPreferences>('get_recording_preferences');
        await invoke('set_recording_preferences', {
          preferences: { ...preferences, [field]: device },
        });
      } catch (error) {
        console.warn(`[SetupCheckPanel] Failed to save ${field}:`, error);
      }
    },
    [],
  );

  const runCheck = useCallback(async () => {
    if (!mountedRef.current) return;

    setResult(null);
    setLevels({});
    setManualFixes([]);
    setPhase('preparing');

    try {
      unlistenLevelRef.current = await onSetupCheckLevel((level) => {
        if (!mountedRef.current) return;

        setLevels((previous) => ({ ...previous, [level.channel]: level }));
        // The first level is the only signal that the model finished loading and
        // the capture window has opened.
        setPhase((previous) => (previous === 'preparing' ? 'capturing' : previous));

        // Nothing announces the end of the capture, so mark the handover to the
        // model ourselves — otherwise both meters simply stop and the wait for
        // the transcript looks like a hang.
        if (!captureTimerRef.current) {
          const remainingMs = Math.max(0, level.durationMs - level.elapsedMs);
          captureTimerRef.current = setTimeout(() => {
            if (!mountedRef.current) return;
            setPhase((previous) => (previous === 'capturing' ? 'transcribing' : previous));
          }, remainingMs);
        }
      });
    } catch (error) {
      // Without the meters the check still produces its three verdicts, which is
      // the part the user came for.
      console.warn('[SetupCheckPanel] Level meters unavailable:', error);
    }

    const outcome = await startSetupCheck({
      micDeviceName: selectedMic,
      systemDeviceName: selectedSystem,
      durationSecs: SETUP_CHECK_DURATION_SECONDS,
    });

    stopLevelUpdates();
    if (!mountedRef.current) return;

    setResult(outcome);
    setPhase('done');
    onOutcomeRef.current?.(summariseSetupCheck(outcome), outcome);
  }, [selectedMic, selectedSystem, stopLevelUpdates]);

  // Kept in a ref so the one-shot auto-start below does not re-fire every time
  // the user picks a different device.
  const runCheckRef = useRef(runCheck);
  runCheckRef.current = runCheck;

  useEffect(() => {
    let cancelled = false;

    const initialise = async () => {
      let inputs: AudioDevice[] = [];
      let outputs: AudioDevice[] = [];
      try {
        ({ inputs, outputs } = await loadDevices());
      } catch (error) {
        console.error('[SetupCheckPanel] Failed to list audio devices:', error);
        if (!cancelled) {
          setDeviceError('Minutes could not list the audio devices on this computer.');
        }
      }

      try {
        const preferences = await invoke<RecordingPreferences>('get_recording_preferences');
        // Saved selections carry the " (input)" / " (output)" suffix while
        // enumeration returns raw names, so compare against the suffixed form —
        // and fall back to the system default for a device that has been
        // unplugged since it was chosen.
        const mic = preferences.preferred_mic_device;
        if (!cancelled && mic && inputs.some((device) => micDeviceValue(device) === mic)) {
          setSelectedMic(mic);
        }
        const system = preferences.preferred_system_device;
        if (!cancelled && system && outputs.some((device) => systemDeviceValue(device) === system)) {
          setSelectedSystem(system);
        }
      } catch (error) {
        console.warn('[SetupCheckPanel] Failed to read recording preferences:', error);
      }

      // Started only after the saved devices are in hand, so an automatic run
      // opens the devices the user chose rather than the defaults.
      if (!cancelled && autoStart) {
        runCheckRef.current();
      }
    };

    initialise();
    return () => {
      cancelled = true;
    };
  }, [autoStart, loadDevices]);

  const handleRefreshDevices = async () => {
    setRefreshing(true);
    try {
      await loadDevices();
    } catch (error) {
      console.error('[SetupCheckPanel] Failed to refresh audio devices:', error);
      setDeviceError('Minutes could not list the audio devices on this computer.');
    } finally {
      setRefreshing(false);
    }
  };

  const handleMicChange = (value: string) => {
    const device = value === 'default' ? null : value;
    setSelectedMic(device);
    savePreferredDevice('preferred_mic_device', device);
    // Announced whether or not that write reaches disk. The in-memory selection
    // is what this session's recording actually opens, so it has to name the
    // device the check is about to test even when the preference cannot be
    // saved for next launch.
    onDevicesChanged?.({ micDevice: device, systemDevice: selectedSystem });
  };

  const handleSystemChange = (value: string) => {
    const device = value === 'default' ? null : value;
    setSelectedSystem(device);
    savePreferredDevice('preferred_system_device', device);
    onDevicesChanged?.({ micDevice: selectedMic, systemDevice: device });
  };

  const focusPicker = (channel: SetupCheckChannel) => {
    const trigger = channel === 'microphone' ? micTriggerRef.current : systemTriggerRef.current;
    trigger?.scrollIntoView({ behavior: 'smooth', block: 'center' });
    trigger?.focus();
  };

  const showManualFix = (kind: SetupCheckFixKind) => {
    setManualFixes((previous) => (previous.includes(kind) ? previous : [...previous, kind]));
  };

  /**
   * `channel` says which picker "choose a different device" means; the model card
   * passes none because its fixes are never about a device.
   */
  const handleFix = async (fix: SetupCheckFix, channel?: SetupCheckChannel) => {
    switch (fix.kind) {
      case 'retry':
        await runCheck();
        break;
      case 'choose-device':
        focusPicker(channel ?? 'microphone');
        break;
      case 'retry-download':
        if (onRetryDownload) onRetryDownload();
        else showManualFix(fix.kind);
        break;
      case 'open-mic-settings':
      case 'open-screen-recording-settings': {
        const opened = await applySetupCheckFix(fix.kind);
        if (!opened) showManualFix(fix.kind);
        break;
      }
    }
  };

  const micLevel = levels.microphone;
  const systemLevel = levels.system_audio;

  // Both channels are captured over one window, so either meter's clock is the
  // run's clock; take the further along of the two in case one arrives late.
  const elapsedMs = Math.max(micLevel?.elapsedMs ?? 0, systemLevel?.elapsedMs ?? 0);
  const windowMs = Math.max(micLevel?.durationMs ?? 0, systemLevel?.durationMs ?? 0);
  const elapsedPercent = windowMs > 0 ? Math.min(100, (elapsedMs / windowMs) * 100) : 0;
  const secondsLeft =
    windowMs > 0 ? Math.max(0, Math.ceil((windowMs - elapsedMs) / 1000)) : SETUP_CHECK_DURATION_SECONDS;

  const micDisplayName = displayDeviceName(selectedMic, DEFAULT_MIC_LABEL);
  const systemDisplayName = displayDeviceName(selectedSystem, DEFAULT_SYSTEM_LABEL);

  const modelDescription = useMemo(
    () => (result ? describeModelOutcome(result.model) : null),
    [result],
  );

  return (
    <div className={cn('w-full space-y-4', className)}>
      {/* Devices */}
      <div className="bg-surface-raised rounded-xl border border-hairline p-5 space-y-4">
        <div className="flex items-center justify-between">
          <p className="text-sm font-medium text-ink">Devices</p>
          <button
            onClick={handleRefreshDevices}
            disabled={refreshing || isRunning}
            title="Refresh the list of audio devices"
            className="h-8 w-8 p-0 inline-flex items-center justify-center rounded-md transition-colors hover:bg-surface-2 disabled:pointer-events-none disabled:opacity-50"
          >
            <RefreshCw className={cn('h-4 w-4 text-ink-muted', refreshing && 'animate-spin')} />
          </button>
        </div>

        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <Mic className="h-4 w-4 text-ink-muted" />
            <Label htmlFor="setup-check-mic-device" className="text-sm font-medium text-ink">
              Microphone
            </Label>
          </div>
          <Select
            value={selectedMic || 'default'}
            onValueChange={handleMicChange}
            disabled={isRunning}
          >
            <SelectTrigger id="setup-check-mic-device" ref={micTriggerRef} className="w-full">
              <SelectValue placeholder="Select a microphone" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="default">{DEFAULT_MIC_LABEL}</SelectItem>
              {micDevices.map((device) => (
                <SelectItem key={device.name} value={micDeviceValue(device)}>
                  {device.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {!deviceError && micDevices.length === 0 && (
            <p className="text-xs text-ink-muted">No microphone devices found</p>
          )}
        </div>

        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <Speaker className="h-4 w-4 text-ink-muted" />
            <Label htmlFor="setup-check-system-device" className="text-sm font-medium text-ink">
              System audio
            </Label>
          </div>
          <Select
            value={selectedSystem || 'default'}
            onValueChange={handleSystemChange}
            disabled={isRunning}
          >
            <SelectTrigger id="setup-check-system-device" ref={systemTriggerRef} className="w-full">
              <SelectValue placeholder="Select a system audio device" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="default">{DEFAULT_SYSTEM_LABEL}</SelectItem>
              {systemDevices.map((device) => (
                <SelectItem key={device.name} value={systemDeviceValue(device)}>
                  {device.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {!deviceError && systemDevices.length === 0 && (
            <p className="text-xs text-ink-muted">No system audio devices found</p>
          )}
        </div>

        {deviceError && <p className="text-xs text-error">{deviceError}</p>}
      </div>

      {/* The model, checked first and on its own, because a model that will not
          load is why both channels come back untested. */}
      {phase !== 'done' && (
        <div className="bg-surface-raised rounded-xl border border-hairline p-5">
          <div className="flex items-center gap-3">
            {phase === 'preparing' ? (
              <Loader2 className="h-4 w-4 animate-spin text-ink-muted" />
            ) : (
              <Cpu className="h-4 w-4 text-ink-muted" />
            )}
            <p className="text-sm text-ink">
              {phase === 'idle'
                ? 'Minutes will load the transcription model first, then listen on both channels at once.'
                : phase === 'preparing'
                  ? 'Loading the transcription model...'
                  : 'The transcription model is loaded.'}
            </p>
          </div>
        </div>
      )}

      {/* Capture */}
      {isRunning ? (
        <div className="bg-surface-raised rounded-xl border border-hairline p-5 space-y-4">
          {phase === 'transcribing' ? (
            <div className="flex items-center gap-3 text-sm text-ink">
              <Loader2 className="h-4 w-4 animate-spin text-ink-muted" />
              Turning what Minutes heard into text...
            </div>
          ) : (
            <>
              <p className="text-sm text-ink">
                {phase === 'capturing'
                  ? 'Listening on both channels — say a sentence out loud, anything at all.'
                  : 'Getting ready to listen...'}
              </p>

              {/* Both meters are always drawn, even for a channel sending nothing:
                  a missing meter reads as a crash, a meter sitting at zero reads
                  as silence, and silence is the thing the user needs to see. */}
              <div className="space-y-4">
                <ChannelMeter
                  icon={<Mic className="h-4 w-4 text-ink-muted" />}
                  title="Microphone"
                  deviceName={micDisplayName}
                  level={micLevel}
                  activeHint="Picking up your voice"
                  idleHint="Not hearing anything yet"
                />
                <ChannelMeter
                  icon={<Speaker className="h-4 w-4 text-ink-muted" />}
                  title="System audio"
                  deviceName={systemDisplayName}
                  level={systemLevel}
                  activeHint="Picking up what your computer is playing"
                  idleHint="Your computer is quiet"
                />
              </div>

              <div className="space-y-2">
                <div className="w-full h-2 bg-surface-2 rounded-full overflow-hidden">
                  <div
                    className="h-full bg-brand rounded-full transition-all duration-150"
                    style={{ width: `${elapsedPercent}%` }}
                  />
                </div>
                <p className="text-sm text-ink-muted">{secondsLeft}s left</p>
              </div>

              {/* Offered only while something is still being captured: once the
                  model has the audio there is nothing left to stop, and a button
                  that does nothing is worse than no button. */}
              <Button variant="gray" onClick={() => cancelSetupCheck()} className="w-full h-11">
                <Square className="w-4 h-4" />
                Stop
              </Button>
            </>
          )}
        </div>
      ) : (
        <div className="bg-surface-raised rounded-xl border border-hairline p-5 space-y-4">
          <p className="text-sm text-ink">
            {phase === 'idle'
              ? `Minutes will listen for ${SETUP_CHECK_DURATION_SECONDS} seconds. Say a sentence out loud — the weather, what you had for lunch, anything.`
              : 'Run it again whenever you are ready to say a few more words.'}
          </p>
          <Button
            onClick={runCheck}
            className="w-full h-11 bg-brand hover:bg-brand text-brand-foreground"
          >
            <Mic className="w-4 h-4" />
            {phase === 'idle' ? 'Start the check' : 'Run the check again'}
          </Button>
        </div>
      )}

      {/* Three results, never one. Each card answers for its own part of the
          check and says nothing about the others. */}
      <AnimatePresence>
        {result && modelDescription && phase === 'done' && (
          <motion.div
            initial={{ opacity: 0, y: -10 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: -10 }}
            transition={{ duration: 0.3, ease: 'easeOut' }}
            className="space-y-4"
          >
            <ReportCard
              icon={<Cpu className="h-4 w-4 text-ink-muted" />}
              tone={MODEL_TONES[result.model.outcome]}
              description={modelDescription}
              detail={result.model.detail}
              manualFixes={manualFixes}
              onFix={(fix) => handleFix(fix)}
            />

            <ChannelCard
              icon={<Mic className="h-4 w-4 text-ink-muted" />}
              report={result.microphone}
              tone={MICROPHONE_TONES[result.microphone.outcome]}
              manualFixes={manualFixes}
              onFix={handleFix}
            />

            <ChannelCard
              icon={<Speaker className="h-4 w-4 text-ink-muted" />}
              report={result.systemAudio}
              tone={SYSTEM_AUDIO_TONES[result.systemAudio.outcome]}
              manualFixes={manualFixes}
              onFix={handleFix}
            />
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

interface ChannelMeterProps {
  icon: React.ReactNode;
  title: string;
  deviceName: string;
  level?: SetupCheckLevel;
  activeHint: string;
  idleHint: string;
}

function ChannelMeter({ icon, title, deviceName, level, activeHint, idleHint }: ChannelMeterProps) {
  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2 min-w-0">
          {icon}
          <span className="text-sm font-medium text-ink">{title}</span>
          <span className="text-xs text-ink-muted truncate">{deviceName}</span>
        </div>
        <span className="text-xs text-ink-muted shrink-0">
          {level?.isActive ? activeHint : idleHint}
        </span>
      </div>
      <AudioLevelMeter
        rmsLevel={level?.rms ?? 0}
        peakLevel={level?.peak ?? 0}
        isActive={level?.isActive ?? false}
        deviceName={deviceName}
        size="large"
      />
    </div>
  );
}

interface ReportCardProps {
  icon: React.ReactNode;
  tone: CardTone;
  description: SetupCheckDescription;
  detail: string | null;
  transcript?: string | null;
  manualFixes: SetupCheckFixKind[];
  onFix: (fix: SetupCheckFix) => void;
}

function ReportCard({
  icon,
  tone,
  description,
  detail,
  transcript,
  manualFixes,
  onFix,
}: ReportCardProps) {
  const fix = description.fix;
  const manualInstruction =
    fix && manualFixes.includes(fix.kind) ? MANUAL_FIX_INSTRUCTIONS[fix.kind] : undefined;

  return (
    <div className={cn('rounded-xl border p-5 space-y-3', TONE_CLASSES[tone])}>
      <div className="flex items-center gap-2">
        {icon}
        <h3 className="font-medium text-ink">{description.title}</h3>
      </div>

      <p className="text-sm text-ink">{description.message}</p>

      {transcript && (
        <blockquote className="rounded-lg bg-surface-raised border border-hairline p-4 text-base text-ink whitespace-pre-wrap break-words">
          {transcript}
        </blockquote>
      )}

      {manualInstruction && <p className="text-sm text-ink-muted">{manualInstruction}</p>}

      {/* Raw error text, kept small and last: it is here for a support
          conversation, not for the user to have to read. */}
      {detail && <p className="text-xs text-ink-muted break-words">{detail}</p>}

      {fix && (
        <Button
          onClick={() => onFix(fix)}
          variant="gray"
          className="w-full h-11"
        >
          {fix.label}
        </Button>
      )}
    </div>
  );
}

interface ChannelCardProps {
  icon: React.ReactNode;
  report: ChannelReport;
  tone: CardTone;
  manualFixes: SetupCheckFixKind[];
  onFix: (fix: SetupCheckFix, channel: SetupCheckChannel) => void;
}

function ChannelCard({ icon, report, tone, manualFixes, onFix }: ChannelCardProps) {
  return (
    <ReportCard
      icon={icon}
      tone={tone}
      description={describeChannelOutcome(report)}
      detail={report.detail}
      // Reading their own words back is what makes the microphone result
      // believable; the system-audio channel has no transcript to show unless it
      // heard speech, and the wire only fills this in when it did.
      transcript={report.transcript}
      manualFixes={manualFixes}
      onFix={(fix) => onFix(fix, report.channel)}
    />
  );
}
