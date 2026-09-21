import React, { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { Loader2, Mic, RefreshCw, Square } from 'lucide-react';
import { motion, AnimatePresence } from 'framer-motion';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { AudioLevelMeter } from '@/components/AudioLevelMeter';
import { OnboardingContainer } from '../OnboardingContainer';
import { useOnboarding } from '@/contexts/OnboardingContext';
import type { RecordingPreferences } from '@/components/RecordingSettings';
import {
  applyMicCheckFix,
  cancelMicCheck,
  describeMicCheckOutcome,
  onMicCheckLevel,
  startMicCheck,
  MIC_CHECK_DURATION_SECONDS,
  type MicCheckFix,
  type MicCheckLevel,
  type MicCheckResult,
} from '@/lib/micCheck';

interface AudioDevice {
  name: string;
  device_type: 'Input' | 'Output';
}

type CheckPhase = 'idle' | 'recording' | 'transcribing' | 'done';

/** The form the backend and the saved preference both use for a chosen input. */
const deviceValue = (device: AudioDevice) => `${device.name} (input)`;

export function MicCheckStep() {
  const { goToStep, completeOnboarding } = useOnboarding();

  const [isMac, setIsMac] = useState(false);
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [deviceError, setDeviceError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  // null is the system default, matching what the backend does with no device name.
  const [selectedMic, setSelectedMic] = useState<string | null>(null);
  const [phase, setPhase] = useState<CheckPhase>('idle');
  const [level, setLevel] = useState<MicCheckLevel | null>(null);
  const [result, setResult] = useState<MicCheckResult | null>(null);
  const [showManualSettingsSteps, setShowManualSettingsSteps] = useState(false);
  const [finishing, setFinishing] = useState(false);

  const unlistenLevelRef = useRef<UnlistenFn | null>(null);
  const captureTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const micTriggerRef = useRef<HTMLButtonElement>(null);
  const mountedRef = useRef(true);

  const isChecking = phase === 'recording' || phase === 'transcribing';

  useEffect(() => {
    const checkPlatform = async () => {
      try {
        const { platform } = await import('@tauri-apps/plugin-os');
        setIsMac(platform() === 'macos');
      } catch {
        setIsMac(navigator.userAgent.includes('Mac'));
      }
    };

    checkPlatform();
  }, []);

  const loadDevices = useCallback(async () => {
    const all = await invoke<AudioDevice[]>('get_audio_devices');
    const inputs = all.filter((device) => device.device_type === 'Input');
    setDevices(inputs);
    setDeviceError(null);
    return inputs;
  }, []);

  useEffect(() => {
    const initialise = async () => {
      let inputs: AudioDevice[] = [];
      try {
        inputs = await loadDevices();
      } catch (error) {
        console.error('[MicCheckStep] Failed to list audio devices:', error);
        setDeviceError('Minutes could not list the microphones on this computer.');
      }

      try {
        const preferences = await invoke<RecordingPreferences>('get_recording_preferences');
        const preferred = preferences.preferred_mic_device;
        // Saved selections carry the " (input)" suffix while enumeration returns raw names, so
        // compare against the suffixed form — and fall back to the system default for a device
        // that has been unplugged since it was chosen.
        if (preferred && inputs.some((device) => deviceValue(device) === preferred)) {
          setSelectedMic(preferred);
        }
      } catch (error) {
        console.warn('[MicCheckStep] Failed to read recording preferences:', error);
      }
    };

    initialise();
  }, [loadDevices]);

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

  // Leaving the step mid-check would otherwise hold the input device open.
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      stopLevelUpdates();
      cancelMicCheck();
    };
  }, [stopLevelUpdates]);

  /**
   * The preferred mic is written back so the first real meeting opens the device the user just
   * watched work. A preference that will not save is never worth stranding someone on the last
   * screen of onboarding for.
   */
  const savePreferredMic = async (device: string | null) => {
    try {
      const preferences = await invoke<RecordingPreferences>('get_recording_preferences');
      await invoke('set_recording_preferences', {
        preferences: { ...preferences, preferred_mic_device: device },
      });
    } catch (error) {
      console.warn('[MicCheckStep] Failed to save the preferred microphone:', error);
    }
  };

  const runCheck = async () => {
    if (isChecking) return;

    setResult(null);
    setLevel(null);
    setShowManualSettingsSteps(false);
    setPhase('recording');

    try {
      unlistenLevelRef.current = await onMicCheckLevel((update) => setLevel(update));
    } catch (error) {
      // Without the meter the check still produces a verdict, which is the part that matters.
      console.warn('[MicCheckStep] Level meter unavailable:', error);
    }

    // Nothing announces the end of the capture, so mark the handover to the model ourselves —
    // otherwise the meter simply stops and the wait looks like a hang.
    captureTimerRef.current = setTimeout(() => {
      if (mountedRef.current) setPhase('transcribing');
    }, MIC_CHECK_DURATION_SECONDS * 1000);

    const outcome = await startMicCheck({
      deviceName: selectedMic,
      durationSecs: MIC_CHECK_DURATION_SECONDS,
    });

    stopLevelUpdates();
    if (!mountedRef.current) return;

    setResult(outcome);
    setPhase('done');

    if (outcome.outcome === 'transcribed') {
      savePreferredMic(selectedMic);
    }
  };

  const handleCancel = async () => {
    await cancelMicCheck();
  };

  const handleDeviceChange = (value: string) => {
    const device = value === 'default' ? null : value;
    setSelectedMic(device);
    savePreferredMic(device);
  };

  const handleRefreshDevices = async () => {
    setRefreshing(true);
    try {
      await loadDevices();
    } catch (error) {
      console.error('[MicCheckStep] Failed to refresh audio devices:', error);
      setDeviceError('Minutes could not list the microphones on this computer.');
    } finally {
      setRefreshing(false);
    }
  };

  const focusDevicePicker = () => {
    micTriggerRef.current?.scrollIntoView({ behavior: 'smooth', block: 'center' });
    micTriggerRef.current?.focus();
  };

  const handleFix = async (fix: MicCheckFix) => {
    switch (fix.kind) {
      case 'retry':
        await runCheck();
        break;
      case 'choose-device':
        focusDevicePicker();
        break;
      case 'retry-download':
        goToStep(3);
        break;
      case 'open-mic-settings': {
        const opened = await applyMicCheckFix(fix.kind);
        if (!opened) setShowManualSettingsSteps(true);
        break;
      }
    }
  };

  const handleFinish = async () => {
    setFinishing(true);
    try {
      await completeOnboarding();
      window.location.reload();
    } catch (error) {
      console.error('[MicCheckStep] Failed to complete onboarding:', error);
      toast.error('Could not finish setup', {
        description: 'Please try again in a moment.',
      });
      setFinishing(false);
    }
  };

  const elapsedPercent = level && level.durationMs > 0
    ? Math.min(100, (level.elapsedMs / level.durationMs) * 100)
    : 0;
  const secondsLeft = level
    ? Math.max(0, Math.ceil((level.durationMs - level.elapsedMs) / 1000))
    : MIC_CHECK_DURATION_SECONDS;

  const description = result ? describeMicCheckOutcome(result) : null;
  const succeeded = result?.outcome === 'transcribed';

  return (
    <OnboardingContainer
      title="Say something"
      description="Minutes will record a few seconds and read it back to you, so you know it can hear you before your first meeting."
      step={isMac ? 5 : 4}
      totalSteps={isMac ? 5 : 4}
    >
      <div className="flex flex-col items-center space-y-6">
        <div className="w-full max-w-lg space-y-4">
          {/* Microphone picker */}
          <div className="bg-surface-raised rounded-xl border border-hairline p-5 space-y-2">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <Mic className="h-4 w-4 text-ink-muted" />
                <Label htmlFor="mic-check-device" className="text-sm font-medium text-ink">
                  Microphone
                </Label>
              </div>
              <button
                onClick={handleRefreshDevices}
                disabled={refreshing || isChecking}
                title="Refresh the list of microphones"
                className="h-8 w-8 p-0 inline-flex items-center justify-center rounded-md transition-colors hover:bg-surface-2 disabled:pointer-events-none disabled:opacity-50"
              >
                <RefreshCw className={`h-4 w-4 text-ink-muted ${refreshing ? 'animate-spin' : ''}`} />
              </button>
            </div>

            <Select
              value={selectedMic || 'default'}
              onValueChange={handleDeviceChange}
              disabled={isChecking}
            >
              <SelectTrigger id="mic-check-device" ref={micTriggerRef} className="w-full">
                <SelectValue placeholder="Select Microphone" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="default">Default Microphone</SelectItem>
                {devices.map((device) => (
                  <SelectItem key={device.name} value={deviceValue(device)}>
                    {device.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>

            {deviceError ? (
              <p className="text-xs text-error">{deviceError}</p>
            ) : (
              devices.length === 0 && (
                <p className="text-xs text-ink-muted">No microphone devices found</p>
              )
            )}
          </div>

          {/* Capture */}
          <div className="bg-surface-raised rounded-xl border border-hairline p-5 space-y-4">
            {phase === 'recording' ? (
              <>
                <p className="text-sm text-ink">
                  Listening — say a sentence out loud, anything at all.
                </p>

                <AudioLevelMeter
                  rmsLevel={level?.rms ?? 0}
                  peakLevel={level?.peak ?? 0}
                  isActive={level?.isActive ?? false}
                  deviceName={selectedMic ?? 'Default Microphone'}
                  size="large"
                />

                <div className="space-y-2">
                  <div className="w-full h-2 bg-surface-2 rounded-full overflow-hidden">
                    <div
                      className="h-full bg-gradient-to-r from-gray-700 to-gray-900 rounded-full transition-all duration-150"
                      style={{ width: `${elapsedPercent}%` }}
                    />
                  </div>
                  <div className="flex items-center justify-between text-sm text-ink-muted">
                    <span>{secondsLeft}s left</span>
                    <span>
                      {level?.isActive ? 'Picking up your voice' : 'Not hearing anything yet'}
                    </span>
                  </div>
                </div>

                <Button variant="gray" onClick={handleCancel} className="w-full h-11">
                  <Square className="w-4 h-4" />
                  Stop
                </Button>
              </>
            ) : phase === 'transcribing' ? (
              <div className="flex items-center gap-3 text-sm text-ink">
                <Loader2 className="w-4 h-4 animate-spin text-ink-muted" />
                Turning what you said into text...
              </div>
            ) : (
              <>
                <p className="text-sm text-ink">
                  {phase === 'idle'
                    ? `Minutes will listen for ${MIC_CHECK_DURATION_SECONDS} seconds. Say a sentence out loud — the weather, what you had for lunch, anything.`
                    : 'Run it again whenever you are ready to say a few more words.'}
                </p>
                <Button
                  onClick={runCheck}
                  className="w-full h-11 bg-brand hover:bg-brand text-brand-foreground"
                >
                  <Mic className="w-4 h-4" />
                  {phase === 'idle' ? 'Start the check' : 'Run the check again'}
                </Button>
              </>
            )}
          </div>

          {/* Verdict */}
          <AnimatePresence>
            {result && description && phase === 'done' && (
              <motion.div
                initial={{ opacity: 0, y: -10 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0, y: -10 }}
                transition={{ duration: 0.3, ease: 'easeOut' }}
                className={`rounded-xl border p-5 space-y-3 ${
                  succeeded
                    ? 'bg-success-subtle border-success'
                    : 'bg-surface-raised border-hairline'
                }`}
              >
                <h3 className="font-medium text-ink">{description.title}</h3>
                <p className="text-sm text-ink">{description.message}</p>

                {succeeded && result.transcript && (
                  <blockquote className="rounded-lg bg-surface-raised border border-hairline p-4 text-base text-ink whitespace-pre-wrap break-words">
                    {result.transcript}
                  </blockquote>
                )}

                {result.detail && (
                  <p className="text-xs text-ink-muted break-words">{result.detail}</p>
                )}

                {showManualSettingsSteps && (
                  <p className="text-sm text-ink-muted">
                    Minutes could not open the settings for you. Open them yourself: System
                    Settings &rsaquo; Privacy &amp; Security &rsaquo; Microphone on macOS, or
                    Settings &rsaquo; Privacy &amp; security &rsaquo; Microphone on Windows. Switch
                    Minutes on, then run the check again.
                  </p>
                )}

                {succeeded ? (
                  <>
                    <p className="text-sm text-ink-muted">
                      That recording and that transcript were both made on this computer. Nothing
                      was uploaded.
                    </p>
                    <Button
                      onClick={handleFinish}
                      disabled={finishing}
                      className="w-full h-11 bg-brand hover:bg-brand text-brand-foreground"
                    >
                      {finishing ? <Loader2 className="w-4 h-4 animate-spin" /> : 'Finish setup'}
                    </Button>
                  </>
                ) : (
                  <div className="flex flex-col gap-3 pt-1">
                    {description.fix && (
                      <Button
                        onClick={() => handleFix(description.fix!)}
                        className="w-full h-11 bg-brand hover:bg-brand text-brand-foreground"
                      >
                        {description.fix.label}
                      </Button>
                    )}

                    {/* A broken microphone must never be the thing that traps someone in setup. */}
                    <button
                      onClick={handleFinish}
                      disabled={finishing}
                      className="text-sm text-ink-muted hover:text-ink transition-colors disabled:opacity-50"
                    >
                      Skip for now
                    </button>
                  </div>
                )}
              </motion.div>
            )}
          </AnimatePresence>
        </div>
      </div>
    </OnboardingContainer>
  );
}
