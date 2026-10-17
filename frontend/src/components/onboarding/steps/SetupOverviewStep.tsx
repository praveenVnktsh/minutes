import React from 'react';
import { Info } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { OnboardingContainer } from '../OnboardingContainer';
import { useOnboarding } from '@/contexts/OnboardingContext';
import { formatBytes } from '@/lib/download-display';
import { onboardingDownloadTotal } from '@/lib/liveNotesOnboarding';
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";

/** Shown until the catalogue answers; a guessed size on a metered connection is worse than none. */
const SIZE_UNKNOWN = 'Checking size…';

export function SetupOverviewStep() {
  const {
    goNext,
    parakeetSizeBytes,
    summaryModelSizeBytes,
    liveNotesModelSizeBytes,
    liveNotesOptIn,
    setLiveNotesOptIn,
    selectedSummaryModel,
    startBackgroundDownloads,
  } = useOnboarding();
  const steps = [
    {
      number: 1,
      type: 'transcription',
      title: 'Download Transcription Engine',
      sizeBytes: parakeetSizeBytes,
      note: 'Required — setup waits for this one.',
    },
    {
      number: 2,
      type: 'summarization',
      title: 'Download Summarization Engine',
      sizeBytes: summaryModelSizeBytes,
      note: 'Continues in the background — you can start using Minutes before it finishes.',
    },
    {
      number: 3,
      type: 'live-notes',
      title: 'Live Notes Engine (optional)',
      sizeBytes: liveNotesModelSizeBytes,
      note: 'A small, faster model for notes while you record. Downloads after the two above finish.',
    },
  ];

  // A total is only honest once its numbers are real, so a half-known total is no total.
  const total = onboardingDownloadTotal(
    parakeetSizeBytes,
    summaryModelSizeBytes,
    liveNotesModelSizeBytes,
    liveNotesOptIn,
  );
  const totalLabel = total !== null ? formatBytes(total) : SIZE_UNKNOWN;

  const handleContinue = () => {
    // This click is where the transfer is authorised, but nothing about it should hold the
    // user here: fire the downloads and move on. A missing recommendation only delays the
    // summary model, which the next screen picks up, and the next screen also reports and
    // retries failures — so navigate either way.
    startBackgroundDownloads({
      includeParakeet: true,
      includeSummary: true,
      summaryModel: selectedSummaryModel,
    }).catch((error) => {
      console.error('[SetupOverviewStep] Failed to start downloads:', error);
    });
    goNext();
  };

  return (
    <OnboardingContainer
      title="Setup Overview"
      description="Minutes requires that you download the Transcription & Summarization AI models for the software to work."
      step={2}
      totalSteps={4}
    >
      <div className="flex flex-col items-center space-y-10">
        {/* Steps Card */}
        <div className="w-full max-w-md bg-surface-raised rounded-lg border border-hairline p-4">
          <div className="space-y-4">
            {steps.map((step, idx) => {
              return (
                <div
                  key={step.number}
                  className={`flex items-start gap-4 p-1`}
                >
                  <div className="flex-1 ml-1">
                    <h3 className="font-medium text-ink flex items-center gap-2">
                        Step {step.number} :  {step.title}

                        {step.type === "summarization" && (
                            <TooltipProvider>
                            <Tooltip>
                                <TooltipTrigger asChild>
                                <button className="text-ink-subtle hover:text-ink-muted">
                                    <Info className="w-4 h-4" />
                                </button>
                                </TooltipTrigger>
                                <TooltipContent className="max-w-xs text-sm">
                                You can also select external AI providers like OpenAI, Claude, or
                                Ollama for summary generation in settings.
                                </TooltipContent>
                            </Tooltip>
                            </TooltipProvider>
                        )}
                        </h3>
                    <p className="mt-1 text-sm text-ink-muted">
                      {step.sizeBytes !== null ? formatBytes(step.sizeBytes) : SIZE_UNKNOWN} ·{' '}
                      {step.note}
                    </p>
                    {step.type === 'live-notes' && (
                      <label className="mt-2 flex cursor-pointer items-center gap-2 text-sm text-ink">
                        <input
                          type="checkbox"
                          checked={liveNotesOptIn}
                          onChange={(event) => setLiveNotesOptIn(event.target.checked)}
                          className="rounded border-hairline text-ink focus:ring-2 focus:ring-focus"
                        />
                        <span className="select-none">Download the live notes engine</span>
                      </label>
                    )}
                  </div>
                </div>
              );
            })}
          </div>

          <div className="mt-4 pt-3 border-t border-hairline flex items-baseline justify-between px-1">
            <span className="text-sm text-ink-muted">Total download</span>
            <span className="text-sm font-medium text-ink">{totalLabel}</span>
          </div>
        </div>


        {/* CTA Section */}
        <div className="w-full max-w-xs space-y-4">
          <Button
            onClick={handleContinue}
            className="w-full h-11 bg-brand hover:bg-brand text-brand-foreground"
          >
            Start Download
          </Button>
          <div className="text-center">
            <a
              href="https://github.com/praveenvnktsh/minutes/issues"
              target="_blank"
              rel="noopener noreferrer"
              className="text-xs text-ink-muted hover:underline"
            >
              Report issues on GitHub
            </a>
          </div>
        </div>
      </div>
    </OnboardingContainer>
  );
}
