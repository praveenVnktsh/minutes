"use client";

import { MeetingSummary, Summary, Transcript } from '@/types';
import { BlockNoteSummaryView, BlockNoteSummaryViewRef } from '@/components/AISummary/BlockNoteSummaryView';
import { EmptyStateSummary } from '@/components/EmptyStateSummary';
import { ModelConfig } from '@/components/ModelSettingsModal';
import Analytics from '@/lib/analytics';
import { RefObject } from 'react';
import { hasVisibleSummaryContent } from '@/lib/summary-content';
import { Button } from '@/components/ui/button';
import { Loader2, Square } from 'lucide-react';

interface SummaryPanelProps {
  meeting: {
    id: string;
    title: string;
    created_at: string;
  };
  meetingTitle: string;
  isSummaryDirty: boolean;
  summaryRef: RefObject<BlockNoteSummaryViewRef>;
  isSaving: boolean;
  onCopySummary: () => Promise<void>;
  aiSummary: MeetingSummary | null;
  summaryStatus: 'idle' | 'processing' | 'summarizing' | 'regenerating' | 'completed' | 'error';
  transcripts: Transcript[];
  modelConfig: ModelConfig;
  onGenerateSummary: (customPrompt: string) => Promise<void>;
  onStopGeneration: () => void;
  customPrompt: string;
  onSaveSummary: (summary: MeetingSummary) => Promise<void>;
  onSummaryChange: (summary: Summary) => void;
  onDirtyChange: (isDirty: boolean) => void;
  summaryError: string | null;
  onRegenerateSummary: () => Promise<void>;
  getSummaryStatusMessage: (status: 'idle' | 'processing' | 'summarizing' | 'regenerating' | 'completed' | 'error') => string;
  availableTemplates: Array<{ id: string, name: string, description: string }>;
  selectedTemplate: string;
  onTemplateSelect: (templateId: string, templateName: string) => void;
  isModelConfigLoading?: boolean;
  onOpenModelSettings?: () => void;
}

export function SummaryPanel({
  meeting,
  meetingTitle,
  summaryRef,
  aiSummary,
  summaryStatus,
  modelConfig,
  onGenerateSummary,
  onStopGeneration,
  customPrompt,
  onSaveSummary,
  onSummaryChange,
  onDirtyChange,
  summaryError,
  onRegenerateSummary,
  getSummaryStatusMessage,
  transcripts,
  onOpenModelSettings,
}: SummaryPanelProps) {
  const isSummaryLoading = summaryStatus === 'processing' || summaryStatus === 'summarizing' || summaryStatus === 'regenerating';
  const hasSummary = hasVisibleSummaryContent(aiSummary);

  return (
    <div className="flex-1 min-w-0 flex flex-col bg-[var(--surface-0)] overflow-hidden h-full w-full">
      {!hasSummary ? (
        isSummaryLoading ? (
          <div className="flex flex-1 flex-col items-center justify-center gap-4" role="status">
            <Loader2 className="h-8 w-8 animate-spin text-info" />
            <p className="text-sm text-ink-muted">Generating summary…</p>
            <Button variant="outline" size="sm" onClick={onStopGeneration} className="gap-2">
              <Square className="h-3 w-3" /> Stop generation
            </Button>
          </div>
        ) : (
          <div className="relative flex min-h-0 flex-1 flex-col">
            <EmptyStateSummary
              onGenerate={() => onGenerateSummary(customPrompt)}
              hasModel={Boolean(modelConfig.provider && modelConfig.model)}
              isGenerating={isSummaryLoading}
              error={summaryError}
            />
            {!modelConfig.provider || !modelConfig.model ? (
              <Button variant="outline" className="absolute bottom-8 left-1/2 -translate-x-1/2" onClick={() => onOpenModelSettings?.()}>
                Set up summary model
              </Button>
            ) : null}
          </div>
        )
      ) : (
        <div className="flex-1 overflow-y-auto overflow-x-auto min-h-0">
          {isSummaryLoading && (
            <div className="sticky top-0 z-10 mx-auto flex max-w-[860px] items-center justify-between rounded-lg bg-info-soft px-3 py-2 text-sm text-info" role="status">
              <span className="flex items-center gap-2"><Loader2 className="h-4 w-4 animate-spin" /> Regenerating. Your current notes remain available.</span>
              <Button variant="ghost" size="sm" onClick={onStopGeneration}>Stop</Button>
            </div>
          )}
          <div className="meeting-notes-editor mx-auto w-full max-w-[860px] px-10 pb-24 pt-6">
            <BlockNoteSummaryView
              ref={summaryRef}
              summaryData={aiSummary}
              onSave={onSaveSummary}
              onSummaryChange={onSummaryChange}
              onDirtyChange={onDirtyChange}
              status={summaryStatus}
              error={summaryError}
              onRegenerateSummary={() => {
                Analytics.trackButtonClick('regenerate_summary', 'meeting_details');
                onRegenerateSummary();
              }}
              meeting={{
                id: meeting.id,
                title: meetingTitle,
                created_at: meeting.created_at
              }}
            />
          </div>
          {summaryStatus === 'error' && (
            <div className="mx-10 mb-8 mt-4 rounded-xl bg-error-soft p-3 text-error" role="alert">
              <p className="text-sm font-medium">{getSummaryStatusMessage(summaryStatus)}</p>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
