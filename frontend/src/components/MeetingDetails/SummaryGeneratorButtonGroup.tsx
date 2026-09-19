"use client";

import { ModelConfig, ModelSettingsModal } from '@/components/ModelSettingsModal';
import {
  Dialog,
  DialogContent,
  DialogTrigger,
  DialogTitle,
} from "@/components/ui/dialog"
import { VisuallyHidden } from "@/components/ui/visually-hidden"
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Sparkles, Settings, FileText, Check, Square } from 'lucide-react';
import { useState, useEffect, ReactNode } from 'react';
import { useConfig } from '@/contexts/ConfigContext';

interface SummaryGeneratorButtonGroupProps {
  languageSlot?: ReactNode;
  modelConfig: ModelConfig;
  setModelConfig?: (config: ModelConfig | ((prev: ModelConfig) => ModelConfig)) => void;
  onSaveModelConfig?: (config?: ModelConfig) => Promise<void>;
  onGenerateSummary: (customPrompt: string) => Promise<void>;
  onRegenerateSummary?: () => Promise<void>;
  onStopGeneration: () => void;
  customPrompt: string;
  summaryStatus: 'idle' | 'processing' | 'summarizing' | 'regenerating' | 'completed' | 'error';
  availableTemplates: Array<{ id: string, name: string, description: string }>;
  selectedTemplate: string;
  onTemplateSelect: (templateId: string, templateName: string) => void;
  hasTranscripts?: boolean;
  hasSummary?: boolean;
  isModelConfigLoading?: boolean;
  onOpenModelSettings?: (openFn: () => void) => void;
}

export function SummaryGeneratorButtonGroup({
  modelConfig,
  onGenerateSummary,
  onRegenerateSummary,
  onStopGeneration,
  customPrompt,
  summaryStatus,
  availableTemplates,
  selectedTemplate,
  onTemplateSelect,
  hasTranscripts = true,
  hasSummary = false,
  isModelConfigLoading = false,
  onOpenModelSettings,
  languageSlot
}: SummaryGeneratorButtonGroupProps) {
  const [settingsDialogOpen, setSettingsDialogOpen] = useState(false);
  const { isModelConfigSaving } = useConfig();

  // Expose the function to open the modal via callback registration
  useEffect(() => {
    if (onOpenModelSettings) {
      // Register our open dialog function with the parent by calling the callback
      // This allows the parent to store a reference to this function
      const openDialog = () => {
        console.log('📱 Opening model settings dialog via callback');
        setSettingsDialogOpen(true);
      };

      // Call the parent's callback with our open function
      // Note: This assumes onOpenModelSettings accepts a function parameter
      // We'll need to adjust the signature
      onOpenModelSettings(openDialog);
    }
  }, [onOpenModelSettings]);

  if (!hasTranscripts) {
    return null;
  }

  const isGenerating = summaryStatus === 'processing' || summaryStatus === 'summarizing' || summaryStatus === 'regenerating';
  const selectedTemplateName = availableTemplates.find((template) => template.id === selectedTemplate)?.name ?? 'Template';

  return (
    <div className="flex flex-wrap items-center gap-1.5">
      {languageSlot}

      {/* Settings button */}
      <Dialog open={settingsDialogOpen} onOpenChange={(open) => { if (!isModelConfigSaving) setSettingsDialogOpen(open); }}>
        <DialogTrigger asChild>
          <Button
            variant="ghost"
            size="sm"
            title="Summary Settings"
            className="h-8 max-w-44 shrink-0 gap-1.5 rounded-full border border-hairline px-3 text-xs text-ink-muted hover:bg-surface-2 hover:text-ink"
          >
            <Settings className="h-3.5 w-3.5" />
            <span className="max-w-32 truncate">{modelConfig.model || 'Choose model'}</span>
          </Button>
        </DialogTrigger>
        <DialogContent
          aria-describedby={undefined}
        >
          <VisuallyHidden>
            <DialogTitle>Model Settings</DialogTitle>
          </VisuallyHidden>
          <ModelSettingsModal
            onCommitted={() => setSettingsDialogOpen(false)}
            onCancel={() => setSettingsDialogOpen(false)}
            layout="dialog"
          />
        </DialogContent>
      </Dialog>

      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="sm" className="h-8 max-w-44 gap-1.5 rounded-full border border-hairline px-3 text-xs">
            <FileText className="h-3.5 w-3.5" /> <span className="truncate">{selectedTemplateName}</span>
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          {availableTemplates.map((template) => (
            <DropdownMenuItem key={template.id} onSelect={() => onTemplateSelect(template.id, template.name)}>
              <Check className={`mr-2 h-3.5 w-3.5 ${selectedTemplate === template.id ? 'opacity-100' : 'opacity-0'}`} />
              {template.name}
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
      <Button
        size="sm"
        variant={isGenerating ? 'error' : 'default'}
        disabled={!isGenerating && (isModelConfigLoading || !modelConfig.model)}
        onClick={() => isGenerating
          ? onStopGeneration()
          : void (hasSummary && onRegenerateSummary ? onRegenerateSummary() : onGenerateSummary(customPrompt))}
        className="h-8 gap-1.5 rounded-full px-3 text-xs"
      >
        {isGenerating ? <><Square className="h-3 w-3" /> Stop</> : <><Sparkles className="h-3.5 w-3.5" /> {hasSummary ? 'Re-enhance' : summaryStatus === 'error' ? 'Retry' : 'Generate'}</>}
      </Button>
    </div>
  );
}
