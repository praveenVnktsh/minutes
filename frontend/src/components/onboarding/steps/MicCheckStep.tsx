import React, { useEffect, useState } from 'react';
import { toast } from 'sonner';
import { OnboardingContainer } from '../OnboardingContainer';
import { useOnboarding } from '@/contexts/OnboardingContext';
import { SetupCheckPanel } from '@/components/SetupCheckPanel';
import { skippedSetupCheckRecord } from '@/lib/micCheck';
import type { SetupCheckRecord, SetupCheckResult } from '@/lib/micCheck';
import { Button } from '@/components/ui/button';
import { Loader2 } from 'lucide-react';

/**
 * The last step of onboarding. All of the device pickers, the model row, the
 * two live meters, the two verdict cards, the transcript and the fix buttons
 * live in SetupCheckPanel now — this step only frames the panel, holds the
 * outcome it reports, and decides what "Finish setup" and "Skip for now" do
 * with that outcome.
 */
export function MicCheckStep() {
  const { goToStep, completeOnboarding } = useOnboarding();

  const [isMac, setIsMac] = useState(false);
  const [result, setResult] = useState<SetupCheckResult | null>(null);
  const [record, setRecord] = useState<SetupCheckRecord | null>(null);
  const [finishing, setFinishing] = useState(false);

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

  const finish = async (setupCheck: SetupCheckRecord | null) => {
    setFinishing(true);
    try {
      await completeOnboarding(setupCheck);
      window.location.reload();
    } catch (error) {
      console.error('[MicCheckStep] Failed to complete onboarding:', error);
      toast.error('Could not finish setup', {
        description: 'Please try again in a moment.',
      });
      setFinishing(false);
    }
  };

  const handleFinish = () => finish(record);

  // Available at any time, not only after a failed run: someone with no
  // microphone attached still has to be able to finish setup. Whatever the
  // run did manage to find out is recorded anyway, so the app can re-offer
  // the check from settings later.
  const handleSkip = () => finish(skippedSetupCheckRecord(result));

  return (
    <OnboardingContainer
      title="Hear it back"
      description="Minutes will prove the transcription model loads, that it can hear you, and what it can hear from the meeting side — then read your own words back to you."
      step={isMac ? 5 : 4}
      totalSteps={isMac ? 5 : 4}
    >
      <div className="flex flex-col items-center space-y-6">
        <div className="w-full max-w-lg space-y-4">
          <SetupCheckPanel
            autoStart
            onOutcome={(nextRecord, nextResult) => {
              setRecord(nextRecord);
              setResult(nextResult);
            }}
            onRetryDownload={() => goToStep(3)}
          />

          {record?.status === 'passed' && (
            <p className="text-sm text-ink-muted">
              That recording and that transcript were both made on this computer. Nothing was
              uploaded.
            </p>
          )}

          <div className="flex flex-col gap-3">
            <Button
              onClick={handleFinish}
              disabled={finishing}
              className="w-full h-11 bg-brand hover:bg-brand text-brand-foreground"
            >
              {finishing ? <Loader2 className="w-4 h-4 animate-spin" /> : 'Finish setup'}
            </Button>

            <button
              onClick={handleSkip}
              disabled={finishing}
              className="text-sm text-ink-muted hover:text-ink transition-colors disabled:opacity-50"
            >
              Skip for now
            </button>
          </div>
        </div>
      </div>
    </OnboardingContainer>
  );
}
