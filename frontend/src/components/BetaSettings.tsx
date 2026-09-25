"use client"

import { Switch } from "./ui/switch"
import { FlaskConical, AlertCircle } from "lucide-react"
import { useConfig } from "@/contexts/ConfigContext"
import {
  BetaFeatureKey,
  BETA_FEATURE_NAMES,
  BETA_FEATURE_DESCRIPTIONS
} from "@/types/betaFeatures"

export function BetaSettings() {
  const { betaFeatures, toggleBetaFeature } = useConfig();

  // Define feature order for display (allows custom ordering)
  const featureOrder: BetaFeatureKey[] = ['importAndRetranscribe', 'liveTranscription', 'micEchoCancellation'];

  return (
    <div className="space-y-6">
      <div className="flex items-start gap-3 rounded-lg border border-warning bg-warning-subtle p-4">
        <AlertCircle className="mt-0.5 h-5 w-5 flex-shrink-0 text-warning" />
        <div className="text-sm text-warning">
          <p className="font-medium">Beta Features</p>
          <p className="mt-1">
            These features are still being tested. You may encounter issues, and we appreciate your feedback.
          </p>
        </div>
      </div>

      {/* Dynamic Feature Toggles - Automatically renders all features */}
      {featureOrder.map((featureKey) => (
        <div
          key={featureKey}
          className="bg-surface-raised rounded-lg border border-hairline p-6 shadow-sm"
        >
          <div className="flex items-center justify-between">
            <div className="flex-1">
              <div className="flex items-center gap-2 mb-2">
                <FlaskConical className="h-5 w-5 text-ink-muted" />
                <h3 className="text-lg font-semibold text-ink">
                  {BETA_FEATURE_NAMES[featureKey]}
                </h3>
                <span className="rounded-full bg-warning-subtle px-2 py-0.5 text-xs font-medium text-warning">
                  BETA
                </span>
              </div>
              <p className="text-sm text-ink-muted">
                {BETA_FEATURE_DESCRIPTIONS[featureKey]}
              </p>
            </div>

            <div className="ml-6">
              <Switch
                aria-label={`${BETA_FEATURE_NAMES[featureKey]} beta feature`}
                checked={betaFeatures[featureKey]}
                onCheckedChange={(checked) => toggleBetaFeature(featureKey, checked)}
              />
            </div>
          </div>
        </div>
      ))}

      <div className="rounded-lg border border-info bg-info-subtle p-4">
        <p className="text-sm text-info">
          <strong>Note:</strong> When disabled, beta features will be hidden. Your existing meetings remain unaffected.
        </p>
      </div>
    </div>
  );
}
