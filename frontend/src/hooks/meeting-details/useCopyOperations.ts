import { useCallback } from 'react';
import { invoke as invokeTauri } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import type { MeetingSummary, Transcript } from '@/types';
import type { LiveNotesDocument } from '@/lib/liveNotes';
import { writeClipboardText } from '@/lib/clipboard';
import { legacyAiNotes, notesMarkdown } from '@/lib/aiNotes';
import {
  buildFrontmatter,
  buildMeetingExport,
  exportFileName,
  fetchCompleteTranscripts,
  meetingPeopleNames,
  storedSummaryMarkdown,
  transcriptDurationSeconds,
  transcriptMarkdown,
  type SpeakerName,
  type TranscriptPage,
} from '@/lib/meetingExport';
import { settingsHref } from '@/components/settings/settingsSections';
import {
  DEFAULT_OBSIDIAN_CONFIG,
  type ObsidianExportConfig,
  type ObsidianExportState,
  type SavedObsidianNote,
} from '@/lib/obsidianExport';
import {
  meetingNotesTarget,
  notePersistenceService,
  type NoteDocumentTarget,
} from '@/services/notePersistenceService';

interface UseCopyOperationsProps {
  meeting: { id: string; title?: string; created_at?: string };
  transcripts: Transcript[];
  meetingTitle: string;
  /** The stored Enhanced notes: shown, copied and exported only for a
   * meeting whose notes have no AI lines. */
  aiSummary: MeetingSummary | null;
  /** "Show AI notes": off copies and exports the user's lines only. */
  showAiNotes: boolean;
  notesTarget?: NoteDocumentTarget;
  currentNotesDocument?: LiveNotesDocument | null;
}

/** Linked attendees plus named speakers, for the frontmatter. Either source
 * failing only drops its names; the export goes on. */
async function readPeopleNames(meetingId: string): Promise<string[]> {
  const [attendees, speakers] = await Promise.all([
    invokeTauri<{ name: string }[]>('meeting_attendees_get', { meetingId }).catch((error) => {
      console.warn('Could not read attendees for export:', error);
      return [];
    }),
    invokeTauri<SpeakerName[]>('get_speaker_identities', { meetingId }).catch((error) => {
      console.warn('Could not read speaker names for export:', error);
      return [];
    }),
  ]);
  return meetingPeopleNames(
    Array.isArray(attendees) ? attendees.map((person) => person.name) : [],
    Array.isArray(speakers) ? speakers : [],
  );
}

/** Tags and the transcript toggle for a Downloads export. The export doesn't
 * need Obsidian, so unreadable settings fall back to the defaults. */
async function readExportConfig(): Promise<ObsidianExportConfig> {
  try {
    const state = await invokeTauri<ObsidianExportState | undefined>('get_obsidian_export_config');
    return state?.config ?? DEFAULT_OBSIDIAN_CONFIG;
  } catch (error) {
    console.warn('Could not read export settings; using defaults:', error);
    return DEFAULT_OBSIDIAN_CONFIG;
  }
}

async function readAllTranscripts(meetingId: string): Promise<Transcript[]> {
  return fetchCompleteTranscripts(meetingId, (args) => invokeTauri<TranscriptPage>(
    'api_get_meeting_transcripts',
    args,
  ));
}

export function useCopyOperations({
  meeting,
  meetingTitle,
  aiSummary,
  showAiNotes,
  notesTarget,
  currentNotesDocument,
  onOpenSettings,
}: UseCopyOperationsProps & {
  /** Opens a settings page (for "Choose an Obsidian vault first"). */
  onOpenSettings?: (href: string) => void;
}) {
  const title = meetingTitle || meeting.title || 'Untitled meeting';

  /** The notes as shown: the document (AI lines only when shown), and an old
   * meeting's Enhanced notes when they are shown below it. */
  const readNotes = useCallback(async (): Promise<{ notes: string; aiNotes: string }> => {
    const target = notesTarget ?? meetingNotesTarget(meeting.id);
    await notePersistenceService.flushNotes(target);
    let document = currentNotesDocument;
    if (document === undefined) {
      const snapshot = await notePersistenceService.loadNotes(target);
      if (snapshot.loadState === 'error') throw snapshot.loadError;
      document = snapshot.document;
    }
    return {
      notes: notesMarkdown(document, showAiNotes),
      aiNotes: legacyAiNotes(document, storedSummaryMarkdown(aiSummary), showAiNotes),
    };
  }, [aiSummary, currentNotesDocument, meeting.id, notesTarget, showAiNotes]);

  const handleCopyTranscript = useCallback(async () => {
    try {
      const allTranscripts = await readAllTranscripts(meeting.id);
      if (allTranscripts.length === 0) {
        toast.error('No transcript available to copy');
        return;
      }

      const date = meeting.created_at ? `\n\n_${new Date(meeting.created_at).toLocaleString()}_` : '';
      await writeClipboardText(`# Transcript: ${title}${date}\n\n${transcriptMarkdown(allTranscripts)}\n`);
      toast.success('Transcript copied to clipboard');
    } catch (error) {
      console.error('Failed to copy transcript:', error);
      toast.error('Failed to copy transcript');
    }
  }, [meeting.created_at, meeting.id, title]);

  const handleCopyNotes = useCallback(async () => {
    try {
      const { notes, aiNotes } = await readNotes();
      const body = [notes.trim(), aiNotes.trim()].filter(Boolean).join('\n\n');
      if (!body) {
        toast.error('No notes to copy');
        return;
      }
      await writeClipboardText(`# ${title}\n\n${body}\n`);
      toast.success('Notes copied');
    } catch (error) {
      console.error('Failed to copy notes:', error);
      toast.error('Failed to copy notes');
    }
  }, [readNotes, title]);

  /** The meeting as Markdown with frontmatter, or null when there's nothing
   * to export. Notes come first: if they can't be read, nothing else runs. */
  const buildExportContents = useCallback(async (
    loadConfig: () => Promise<Pick<ObsidianExportConfig, 'tags' | 'include_transcript'>>,
  ): Promise<string | null> => {
    const { notes, aiNotes } = await readNotes();
    const [config, allTranscripts, people] = await Promise.all([
      loadConfig(),
      readAllTranscripts(meeting.id),
      readPeopleNames(meeting.id),
    ]);
    const transcripts = config.include_transcript ? allTranscripts : [];
    if (!notes.trim() && !aiNotes.trim() && transcripts.length === 0) return null;

    const frontmatter = buildFrontmatter({
      title,
      meetingId: meeting.id,
      createdAt: meeting.created_at ? new Date(meeting.created_at) : undefined,
      durationSeconds: transcriptDurationSeconds(allTranscripts),
      attendees: people,
      tags: config.tags,
      exportedAt: new Date(),
    });
    return buildMeetingExport({
      title,
      notes,
      aiNotes,
      transcripts,
      frontmatter,
      includeTranscript: config.include_transcript,
    });
  }, [meeting.created_at, meeting.id, readNotes, title]);

  const fileName = useCallback(
    () => exportFileName(title, meeting.created_at ? new Date(meeting.created_at) : new Date()),
    [meeting.created_at, title],
  );

  const handleExportMarkdown = useCallback(async () => {
    try {
      const contents = await buildExportContents(readExportConfig);
      if (contents === null) {
        toast.error('Nothing to export yet');
        return;
      }
      const path = await invokeTauri<string>('save_text_export', { fileName: fileName(), contents });
      toast.success('Meeting exported', { description: path });
    } catch (error) {
      console.error('Failed to export meeting:', error);
      toast.error('Failed to export meeting');
    }
  }, [buildExportContents, fileName]);

  const handleSaveToObsidian = useCallback(async () => {
    try {
      const { config, vault_error } = await invokeTauri<ObsidianExportState>('get_obsidian_export_config');
      if (!config.vault_path || vault_error) {
        toast.error(config.vault_path ? 'Your Obsidian vault can’t be found' : 'Choose an Obsidian vault first', {
          description: vault_error ?? undefined,
          action: onOpenSettings
            ? { label: 'Settings', onClick: () => onOpenSettings(settingsHref('integrations')) }
            : undefined,
        });
        return;
      }
      const contents = await buildExportContents(async () => config);
      if (contents === null) {
        toast.error('Nothing to save yet');
        return;
      }
      const saved = await invokeTauri<SavedObsidianNote>('save_obsidian_note', { fileName: fileName(), contents });
      toast.success('Saved to Obsidian', {
        description: saved.version > 1
          ? `${saved.relative_path} (saved as v${saved.version}; earlier versions are unchanged)`
          : saved.relative_path,
        action: {
          label: 'Open in Obsidian',
          onClick: () => {
            invokeTauri('open_obsidian_note', {
              vaultName: saved.vault_name,
              relativePath: saved.relative_path,
            }).catch((error) => toast.error('Could not open Obsidian', { description: String(error) }));
          },
        },
      });
    } catch (error) {
      console.error('Failed to save to Obsidian:', error);
      toast.error('Failed to save to Obsidian', { description: String(error) });
    }
  }, [buildExportContents, fileName, onOpenSettings]);

  return { handleCopyTranscript, handleCopyNotes, handleExportMarkdown, handleSaveToObsidian };
}
