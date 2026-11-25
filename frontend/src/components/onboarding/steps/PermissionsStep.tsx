import React, { useEffect, useState, useCallback } from 'react';
import { Mic, Volume2 } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { OnboardingContainer } from '../OnboardingContainer';
import { PermissionRow } from '../shared';
import { useOnboarding } from '@/contexts/OnboardingContext';
import {
  PERMISSION_PANES,
  checkMicrophonePermission,
  checkSystemAudioPermission,
  openPermissionSettings,
  verdictToStatus,
  type PermissionPane,
} from '@/lib/permissions';
import type { OnboardingPermissions, PermissionReport, PermissionStatus } from '@/types/onboarding';

// The one place this screen writes a settings path in words. `PERMISSION_PANES` holds the
// identifiers `open_system_settings` understands; these are the same two panes spelled the way
// the Settings app spells them, so the user who reads the fallback and the user who watches a
// pane open are being sent to the same screen.
const PANE_LABELS: Record<PermissionPane, string> = {
  [PERMISSION_PANES.microphone]: 'Privacy & Security → Microphone',
  [PERMISSION_PANES.systemAudio]: 'Privacy & Security → Screen & System Audio Recording',
};

export function PermissionsStep() {
  const { setPermissionStatus, setPermissionsSkipped, permissions, goNext } = useOnboarding();
  // One pending flag per row. These are two independent checks against two independent
  // devices, and the microphone one can sit for its full five-second wait, so a click on
  // either row must not disable the other.
  const [isMicrophonePending, setIsMicrophonePending] = useState(false);
  const [isSystemAudioPending, setIsSystemAudioPending] = useState(false);

  // Logs the statuses the rows are already showing and changes nothing. Running a check here
  // would be the same act as asking for the permission — opening the input is what raises the
  // macOS prompt — and a prompt nobody pressed a button for is not something to do on render.
  // Each row asks when its own button is clicked.
  const checkPermissions = useCallback(async () => {
    console.log('[PermissionsStep] Current permission states:');
    console.log(`  - Microphone: ${permissions.microphone}`);
    console.log(`  - System Audio: ${permissions.systemAudio}`);
  }, [permissions.microphone, permissions.systemAudio]);

  // Runs on the first render and again whenever either status changes, so the log follows the
  // rows rather than only describing how the screen opened.
  useEffect(() => {
    checkPermissions();
  }, [checkPermissions]);

  // Sends the user to the setting they have to change by hand, since a denial can only be
  // undone outside the app. `openPermissionSettings` resolves false rather than throwing when
  // there is nothing to open — off macOS, or when the invoke itself failed — and the user still
  // has to reach the setting, so the fallback names it instead of dropping the request.
  const openSettingsFor = async (pane: PermissionPane) => {
    if (await openPermissionSettings(pane)) return;

    toast.error('Could not open the settings screen for you', {
      description: `Allow Minutes there yourself: on macOS it is System Settings → ${PANE_LABELS[pane]}.`,
    });
  };

  // One shape for both checks: run it, log whatever breadcrumb the report carries, and set the
  // row to the status the verdict maps to. The check functions in lib/permissions resolve a
  // report for every outcome, an invoke that never reached the backend included, so there is
  // nothing to catch here — and a throw would in any case say nothing about the grant, which is
  // why this never writes 'denied' of its own accord.
  const runPermissionCheck = async (
    permission: keyof OnboardingPermissions,
    label: string,
    check: () => Promise<PermissionReport>,
    setPending: (pending: boolean) => void
  ) => {
    setPending(true);
    try {
      console.log(`[PermissionsStep] Checking ${label} permission...`);
      const report = await check();
      console.log(
        `[PermissionsStep] ${label} check concluded ${report.verdict}` +
          (report.detail ? `: ${report.detail}` : ' with no further detail')
      );
      setPermissionStatus(permission, verdictToStatus(report.verdict));
    } finally {
      setPending(false);
    }
  };

  // Ask for, or re-ask about, the microphone.
  const handleMicrophoneAction = async () => {
    // A denied row's click opens the pane first, because the grant itself happens in System
    // Settings and not here. The check then runs anyway: someone who already flipped the
    // toggle and came back gets their answer from the same button instead of restarting
    // onboarding. If they have not flipped it yet the check simply reports denied again and
    // the row stays denied, and so stays clickable for the next attempt.
    if (permissions.microphone === 'denied') {
      await openSettingsFor(PERMISSION_PANES.microphone);
    }

    // Every other status runs the same check: 'not_determined' is the first ask, where opening
    // the input is also what raises the macOS prompt, and 'undetermined' is the re-check the
    // row offers as "Check Again".
    await runPermissionCheck(
      'microphone',
      'Microphone',
      checkMicrophonePermission,
      setIsMicrophonePending
    );
  };

  // Ask for, or re-ask about, system audio.
  const handleSystemAudioAction = async () => {
    // Same two-part click as the microphone row, for the same reason.
    if (permissions.systemAudio === 'denied') {
      await openSettingsFor(PERMISSION_PANES.systemAudio);
    }

    // The backend raises the Audio Capture prompt by building a tap and then watches what the
    // tap yields within a bounded wait. It reports 'authorized' only if a non-silent sample
    // arrived; a silent tap comes back 'undetermined', because a Mac with nothing playing
    // produces exactly the silence a missing grant does and the tap cannot tell them apart.
    await runPermissionCheck(
      'systemAudio',
      'System audio',
      checkSystemAudioPermission,
      setIsSystemAudioPending
    );
  };

  const handleContinueToMicCheck = () => {
    goNext();
  };

  const handleSkip = () => {
    setPermissionsSkipped(true);
    handleContinueToMicCheck();
  };

  // Both rows have to have been asked, and neither may have come back denied.
  //
  // Only a denial holds Continue back, on either row, because only a denial is the operating
  // system saying no in as many words. 'undetermined' is not a weaker denial, it is the absence
  // of a verdict, and on both channels it is what a working setup routinely produces: a Mac with
  // nothing playing gives a silent tap, and a muted microphone — or a virtual input like
  // BlackHole left selected as the default, which this app's own system-audio instructions ask
  // users to install — gives callbacks of pure silence. Demanding 'authorized' would leave those
  // users on this screen permanently, since re-checking cannot produce a sample that does not
  // exist. The next step is a live mic check with a meter and a transcript, which is a far better
  // place to find out the microphone is silent than a row that can only keep saying so.
  const asked = (status: PermissionStatus) => status !== 'not_determined' && status !== 'checking';
  const canContinue =
    asked(permissions.microphone) &&
    asked(permissions.systemAudio) &&
    permissions.microphone !== 'denied' &&
    permissions.systemAudio !== 'denied';

  return (
    <OnboardingContainer
      title="Grant Permissions"
      description="Minutes needs access to your microphone and system audio to record meetings"
      step={4}
      hideProgress={true}
      showNavigation={canContinue}
      canGoNext={canContinue}
    >
      <div className="max-w-lg mx-auto space-y-6">
        {/* Permission Rows */}
        <div className="space-y-4">
          {/* Microphone */}
          <PermissionRow
            icon={<Mic className="w-5 h-5" />}
            title="Microphone"
            description="Records your voice during meetings"
            status={permissions.microphone}
            isPending={isMicrophonePending}
            onAction={handleMicrophoneAction}
          />

          {/* System Audio */}
          <PermissionRow
            icon={<Volume2 className="w-5 h-5" />}
            title="System Audio"
            description="Records the other side of the call — whatever plays through your speakers"
            status={permissions.systemAudio}
            isPending={isSystemAudioPending}
            onAction={handleSystemAudioAction}
          />
        </div>

        {/* Action Buttons */}
        <div className="flex flex-col gap-3 pt-4">
          <Button onClick={handleContinueToMicCheck} disabled={!canContinue} className="w-full h-11">
            Continue to Mic Check
          </Button>

          <button
            onClick={handleSkip}
            className="text-sm text-ink-muted hover:text-ink transition-colors"
          >
            I'll do this later
          </button>

          {/* Three ways the gate stays shut, each needing its own sentence. Telling someone
              their microphone is blocked when it is merely unasked sends them to a settings
              screen with nothing wrong in it. */}
          {!canContinue && (
            <p className="text-xs text-center text-muted-foreground">
              {permissions.microphone === 'denied'
                ? 'Minutes cannot record without the microphone. Grant it above, or skip and grant it later in settings.'
                : permissions.systemAudio === 'denied'
                  ? 'System audio is blocked, so meetings would capture your voice but not the people you are talking to. Fix it above, or skip and grant it later in settings.'
                  : 'Check both permissions above to continue. You can also skip and grant them later in settings.'}
            </p>
          )}
        </div>
      </div>
    </OnboardingContainer>
  );
}
