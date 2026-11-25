'use client';

import { useEffect } from 'react';
import { ArrowLeft, Database, FlaskConical, Mic, Settings2, Sparkles, Webhook } from 'lucide-react';
import { useRouter, useSearchParams } from 'next/navigation';
import { AppearanceSetting } from '@/components/AppearanceSetting';
import { BetaSettings } from '@/components/BetaSettings';
import { CalendarSettings } from '@/components/CalendarSettings';
import { DeveloperSettings } from '@/components/DeveloperSettings';
import { PreferenceSettings } from '@/components/PreferenceSettings';
import { RecordingSettings } from '@/components/RecordingSettings';
import { SummaryModelSettings } from '@/components/SummaryModelSettings';
import { TranscriptSettings } from '@/components/TranscriptSettings';
import { WebhookSettings } from '@/components/WebhookSettings';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { isSettingsSection, resolveSettingsSection, settingsHref } from './settingsSections';

const TABS = [
  { value: 'general', label: 'General', icon: Settings2 },
  { value: 'recording', label: 'Recording', icon: Mic },
  { value: 'transcription', label: 'Transcription', icon: Database },
  // The id stays 'summary' so existing settingsHref('summary') deep links keep working.
  { value: 'summary', label: 'AI notes', icon: Sparkles },
  { value: 'integrations', label: 'Integrations', icon: Webhook },
  // Beta and Developer were one tab's worth each; the old 'beta' id still lands here.
  { value: 'developer', label: 'Labs & Developer', icon: FlaskConical },
] as const;

export function SettingsPageContent() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const requestedSection = searchParams.get('section');
  const activeSection = resolveSettingsSection(requestedSection);

  // An unknown section goes to General, and an old alias (beta) to its tab's own id.
  useEffect(() => {
    if (requestedSection !== activeSection) router.replace(settingsHref(activeSection));
  }, [activeSection, requestedSection, router]);

  const changeSection = (section: string) => {
    if (!isSettingsSection(section)) return;
    const next = resolveSettingsSection(section);
    if (next !== activeSection) router.push(settingsHref(next));
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
                      className="gap-2 rounded-none border-b-2 border-transparent bg-transparent px-3 py-3 text-ink-muted shadow-none hover:text-ink data-[state=active]:border-ink data-[state=active]:bg-transparent data-[state=active]:text-ink sm:px-5"
                    >
                      <Icon className="h-4 w-4" />
                      {tab.label}
                    </TabsTrigger>
                  );
                })}
              </TabsList>
            </div>

            <TabsContent value="general" className="mt-6 space-y-6"><AppearanceSetting /><PreferenceSettings /></TabsContent>
            <TabsContent value="recording" className="mt-6"><RecordingSettings /></TabsContent>
            <TabsContent value="transcription" className="mt-6"><TranscriptSettings /></TabsContent>
            <TabsContent value="summary" className="mt-6"><SummaryModelSettings /></TabsContent>
            <TabsContent value="integrations" className="mt-6"><CalendarSettings /><WebhookSettings /></TabsContent>
            <TabsContent value="developer" className="mt-6 space-y-10">
              <section aria-label="Labs" className="space-y-4">
                <h2 className="text-lg font-semibold">Labs</h2>
                <BetaSettings />
              </section>
              <section aria-label="Developer" className="space-y-4">
                <h2 className="text-lg font-semibold">Developer</h2>
                <DeveloperSettings />
              </section>
            </TabsContent>
          </Tabs>
        </div>
      </main>
    </div>
  );
}
