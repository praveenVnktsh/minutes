'use client';

import { useEffect, useState } from 'react';
import { Globe } from 'lucide-react';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { AUTO_VALUE, LANGUAGE_OPTIONS } from '@/lib/summary-languages';
import {
  SUMMARY_LANGUAGE_DEFAULT_KEY,
  readPinnedSummaryLanguageDefault,
  writePinnedSummaryLanguageDefault,
} from '@/lib/summary-language-preferences';

/** Settings → AI notes: the default language AI notes are written in for new meetings. */
export function SummaryLanguageSettings() {
  const [language, setLanguage] = useState<string | null>(() => readPinnedSummaryLanguageDefault());

  useEffect(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.key === SUMMARY_LANGUAGE_DEFAULT_KEY) setLanguage(readPinnedSummaryLanguageDefault());
    };
    window.addEventListener('storage', onStorage);
    return () => window.removeEventListener('storage', onStorage);
  }, []);

  const change = (value: string) => {
    const next = value === AUTO_VALUE ? null : value;
    setLanguage(next);
    writePinnedSummaryLanguageDefault(next);
  };

  return (
    <div className="rounded-lg border border-hairline bg-surface-raised p-6 shadow-sm">
      <div className="mb-2 flex items-center gap-2">
        <Globe size={18} className="text-ink-muted" />
        <h3 className="text-lg font-semibold text-ink">AI notes language</h3>
      </div>
      <p id="summary-language-description" className="mb-4 text-sm text-ink-muted">
        The language AI notes are written in for new meetings. Auto uses the transcript&apos;s main language.
      </p>
      <Select value={language ?? AUTO_VALUE} onValueChange={change}>
        <SelectTrigger
          aria-label="AI notes language"
          aria-describedby="summary-language-description"
          className="max-w-xs focus:ring-2 focus:ring-focus"
        >
          <SelectValue placeholder="Auto" />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={AUTO_VALUE}>Auto</SelectItem>
          {LANGUAGE_OPTIONS.map((option) => (
            <SelectItem key={option.code} value={option.code}>{option.label}</SelectItem>
          ))}
        </SelectContent>
      </Select>
      <p className="mt-3 text-xs text-ink-subtle">
        A language other than English writes the notes from the whole transcript after the meeting, instead of
        building on the notes written live.
      </p>
    </div>
  );
}
