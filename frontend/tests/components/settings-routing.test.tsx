import { afterAll, afterEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

/* eslint-disable @typescript-eslint/no-explicit-any */

const originalNavigation = { ...await import('next/navigation') };
const originalConfig = { ...await import('../../src/contexts/ConfigContext') };
const originalTabs = { ...await import('../../src/components/ui/tabs') };
afterAll(() => {
  mock.module('next/navigation', () => originalNavigation);
  mock.module('../../src/contexts/ConfigContext', () => originalConfig);
  mock.module('../../src/components/ui/tabs', () => originalTabs);
});

let section: string | null = 'general';
const push = mock(() => {});
const replace = mock(() => {});
const back = mock(() => {});
mock.module('next/navigation', () => ({
  useRouter: () => ({ push, replace, back }),
  useSearchParams: () => ({ get: () => section }),
}));
mock.module('../../src/contexts/ConfigContext', () => ({
  useConfig: () => ({ transcriptModelConfig: { provider: 'parakeet', model: 'parakeet' }, setTranscriptModelConfig() {} }),
}));
mock.module('../../src/components/ui/tabs', () => ({
  Tabs: (props: any) => <section data-settings-tabs {...props} />,
  TabsList: (props: any) => <nav {...props} />,
  TabsTrigger: (props: any) => <button {...props} />,
  TabsContent: (props: any) => <div {...props} />,
}));
for (const name of ['PreferenceSettings', 'RecordingSettings', 'SummaryModelSettings', 'BetaSettings', 'WebhookSettings', 'CalendarSettings', 'DebugSettings', 'TranscriptSettings']) {
  mock.module(`../../src/components/${name}`, () => ({ [name]: () => <div>{name}</div> }));
}

const { SettingsPageContent } = await import('../../src/components/settings/SettingsPageContent');
const { resolveSettingsSection, settingsHref } = await import('../../src/components/settings/settingsSections');
let renderer: ReactTestRenderer;
afterEach(() => renderer?.unmount());

describe('canonical settings sections', () => {
  test('uses canonical deep links and falls back from invalid sections', async () => {
    expect(settingsHref('summary')).toBe('/settings?section=summary');
    expect(resolveSettingsSection('not-real')).toBe('general');
    section = 'not-real';
    await act(async () => { renderer = create(<SettingsPageContent />); });
    expect(replace).toHaveBeenCalledWith('/settings?section=general');
  });

  test('selects the URL section and pushes section changes into history', async () => {
    section = 'transcription';
    await act(async () => { renderer = create(<SettingsPageContent />); });
    const tabs = renderer.root.findByProps({ 'data-settings-tabs': true });
    expect(tabs.props.value).toBe('transcription');
    act(() => tabs.props.onValueChange('summary'));
    expect(push).toHaveBeenCalledWith('/settings?section=summary');
  });
});
