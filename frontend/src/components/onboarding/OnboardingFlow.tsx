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
  // 5-Step Onboarding Flow (System-Recommended Models):
  // Step 1: Welcome - Introduce Minutes features
  // Step 2: Setup Overview - Database initialization + show recommended downloads
  // Step 3: Download Progress - Download Parakeet + Summary Model (auto-selected based on platform/RAM)
  // Step 4: Permissions - Request mic + system audio
  // Step 5: Mic Check - Record a few seconds and transcribe them to prove the pipeline works

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
