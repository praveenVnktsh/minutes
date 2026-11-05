import React from 'react';
import { useOnboarding } from '@/contexts/OnboardingContext';
import {
  WelcomeStep,
  PermissionsStep,
  DownloadProgressStep,
  SetupOverviewStep,
  MicCheckStep,
} from './steps';

interface OnboardingFlowProps {
  onComplete: () => void;
}

export function OnboardingFlow({ onComplete }: OnboardingFlowProps) {
  const { currentStep } = useOnboarding();
  // 5-step onboarding flow (the database initializes in the background in OnboardingContext):
  // Step 1: Welcome - introduce Minutes
  // Step 2: Setup Overview - list the recommended models and start their downloads
  // Step 3: Download Progress - wait for the transcription model; the AI notes models continue in the background
  // Step 4: Permissions - request microphone + system audio
  // Step 5: Mic Check - record a few seconds and transcribe them to prove the pipeline works

  return (
    <div className="onboarding-flow">
      {currentStep === 1 && <WelcomeStep />}
      {currentStep === 2 && <SetupOverviewStep />}
      {currentStep === 3 && <DownloadProgressStep />}
      {currentStep === 4 && <PermissionsStep />}
      {currentStep === 5 && <MicCheckStep />}
    </div>
  );
}
