'use client';

import { Label } from '@/components/ui/label';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { useConfig } from '@/contexts/ConfigContext';
import { LANGUAGES } from '@/constants/languages';

/**
 * Settings → Transcription: the language spoken in meetings. Writes the same
 * value RecordingControllerContext reads for the transcript pass after a
 * recording (ConfigContext's selectedLanguage, stored as `primaryLanguage`).
 */
export function TranscriptLanguageSettings() {
  const { selectedLanguage, setSelectedLanguage } = useConfig();
  // "auto-translate" is a legacy value no picker offers any more; keep it
  // listed only for someone who still has it saved.
  const options = LANGUAGES.filter((language) => language.code !== 'auto-translate' || selectedLanguage === 'auto-translate');

  return (
    <div className="pb-6">
      <Label className="mb-1 block text-sm font-medium text-ink">Transcript language</Label>
      <p className="mx-1 mb-2 text-xs text-ink-muted">
        The language spoken in your meetings. Auto-detect suits most meetings; pick a language if
        transcripts come out in the wrong one. Applies from the next recording.
      </p>
      <div className="mx-1">
        <Select value={selectedLanguage || 'auto'} onValueChange={setSelectedLanguage}>
          <SelectTrigger aria-label="Transcript language" className="focus:ring-2 focus:ring-focus">
            <SelectValue placeholder="Auto-detect" />
          </SelectTrigger>
          <SelectContent>
            {options.map((language) => (
              <SelectItem key={language.code} value={language.code}>
                {language.code === 'auto' ? 'Auto-detect' : language.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
    </div>
  );
}
