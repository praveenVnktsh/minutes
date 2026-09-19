'use client';

import { useEffect } from 'react';
import { ArrowLeft, Database, FlaskConical, Mic, Settings2, Sparkles, Webhook } from 'lucide-react';
import { useRouter, useSearchParams } from 'next/navigation';
import { BetaSettings } from '@/components/BetaSettings';
import { CalendarSettings } from '@/components/CalendarSettings';
import { DebugSettings } from '@/components/DebugSettings';
import { PreferenceSettings } from '@/components/PreferenceSettings';
import { RecordingSettings } from '@/components/RecordingSettings';
import { SummaryModelSettings } from '@/components/SummaryModelSettings';
import { TranscriptSettings } from '@/components/TranscriptSettings';
import { WebhookSettings } from '@/components/WebhookSettings';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { useConfig } from '@/contexts/ConfigContext';
import { isSettingsSection, resolveSettingsSection, settingsHref } from './settingsSections';

const TABS = [
  { value: 'general', label: 'General', icon: Settings2 },
  { value: 'recording', label: 'Recording', icon: Mic },
  { value: 'transcription', label: 'Transcription', icon: Database },
  { value: 'summary', label: 'Summary', icon: Sparkles },
  { value: 'integrations', label: 'Integrations', icon: Webhook },
  { value: 'beta', label: 'Beta', icon: FlaskConical },
] as const;

export function SettingsPageContent() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const { transcriptModelConfig, setTranscriptModelConfig } = useConfig();
  const requestedSection = searchParams.get('section');
  const activeSection = resolveSettingsSection(requestedSection);

  useEffect(() => {
    if (!isSettingsSection(requestedSection)) router.replace(settingsHref('general'));
  }, [requestedSection, router]);

  const changeSection = (section: string) => {
    if (isSettingsSection(section) && section !== activeSection) router.push(settingsHref(section));
  };

  return (
    <div className="flex h-screen flex-col bg-surface-2 text-ink">
      <header className="sticky top-0 z-10 border-b border-hairline bg-surface-2">
        <div className="mx-auto flex max-w-6xl items-center gap-4 px-4 py-5 sm:px-8">
          <button type="button" onClick={() => router.back()} className="flex items-center gap-2 rounded-md text-ink-muted hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-focus">
            <ArrowLeft className="h-5 w-5" />
            <span>Back</span>
          </button>
          <h1 className="text-2xl font-bold sm:text-3xl">Settings</h1>
        </div>
      </header>

      <main className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-6xl px-4 pb-10 pt-4 sm:px-8 sm:pt-6">
          <Tabs value={activeSection} onValueChange={changeSection}>
            <div className="-mx-4 overflow-x-auto px-4 sm:mx-0 sm:px-0">
              <TabsList aria-label="Settings sections" className="h-auto min-w-max justify-start rounded-none border-b border-hairline bg-transparent p-0">
                {TABS.map(tab => {
                  const Icon = tab.icon;
                  return (
                    <TabsTrigger
                      key={tab.value}
                      value={tab.value}
                      className="gap-2 rounded-none border-b-2 border-transparent bg-transparent px-3 py-3 text-ink-muted shadow-none hover:text-ink data-[state=active]:border-selected data-[state=active]:bg-transparent data-[state=active]:text-selected sm:px-5"
                    >
                      <Icon className="h-4 w-4" />
                      {tab.label}
                    </TabsTrigger>
                  );
                })}
              </TabsList>
            </div>

            <TabsContent value="general" className="mt-6"><PreferenceSettings /></TabsContent>
            <TabsContent value="recording" className="mt-6"><RecordingSettings /></TabsContent>
            <TabsContent value="transcription" className="mt-6">
              <TranscriptSettings transcriptModelConfig={transcriptModelConfig} setTranscriptModelConfig={setTranscriptModelConfig} />
            </TabsContent>
            <TabsContent value="summary" className="mt-6"><SummaryModelSettings /></TabsContent>
            <TabsContent value="integrations" className="mt-6"><CalendarSettings /><WebhookSettings /></TabsContent>
            <TabsContent value="beta" className="mt-6"><BetaSettings /><div className="mt-6"><DebugSettings /></div></TabsContent>
          </Tabs>
        </div>
      </main>
    </div>
  );
}
