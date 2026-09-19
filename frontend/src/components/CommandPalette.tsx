'use client';

import { useEffect, useState } from 'react';
import {
  FileText,
  Import,
  Link2,
  Mic,
  Search,
  Settings,
  SunMoon,
} from 'lucide-react';
import { toast } from 'sonner';
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
  CommandShortcut,
} from '@/components/ui/command';
import { useSidebar } from '@/components/Sidebar/SidebarProvider';
import { useShell } from '@/contexts/ShellContext';
import { useDebugMode } from '@/hooks/useDebugMode';
import { copyMeetingLink } from '@/lib/clipboard';

const RECENT_MEETINGS_SHOWN = 8;

/**
 * Cmd/Ctrl+K command palette for jumping to meetings and common actions.
 */
export function CommandPalette() {
  const [open, setOpen] = useState(false);
  const { currentMeeting, selectMeetings } = useSidebar();
  const {
    toggleTheme,
    recordingActionLabel,
    runRecordingAction,
    importActionLabel,
    runImportAction,
    navigate,
    openMeeting,
    recordingActionDisabled,
  } = useShell();
  const debugMode = useDebugMode();
  const meetings = selectMeetings({ debugMode });

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== 'k') {
        return;
      }

      // Leave Cmd/Ctrl+K to the focused editor (link/command shortcuts) unless
      // the palette itself is open and should toggle closed.
      const target = event.target as HTMLElement | null;
      const isEditable = !!target && (
        target.tagName === 'INPUT' ||
        target.tagName === 'TEXTAREA' ||
        target.isContentEditable
      );
      if (isEditable && !open) {
        return;
      }

      event.preventDefault();
      setOpen((previous) => !previous);
    };

    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [open]);

  const runCommand = (action: () => void | Promise<void>) => {
    setOpen(false);
    // Let the dialog close before navigating or opening another dialog.
    setTimeout(() => void Promise.resolve(action()).catch((error) => {
      toast.error('Action failed', { description: error instanceof Error ? error.message : String(error) });
    }), 0);
  };

  return (
    <CommandDialog open={open} onOpenChange={setOpen}>
      <CommandInput placeholder="Search meetings or type a command..." />
      <CommandList>
        <CommandEmpty>No results found.</CommandEmpty>

        <CommandGroup heading="Actions">
          <CommandItem disabled={recordingActionDisabled} onSelect={() => runCommand(runRecordingAction)}>
            <Mic />
            {recordingActionLabel}
          </CommandItem>
          <CommandItem onSelect={() => runCommand(() => navigate('/'))}>
            <FileText />
            Open Meetings
          </CommandItem>
          <CommandItem
            onSelect={() =>
              runCommand(async () => {
                await navigate('/');
                setTimeout(() => window.dispatchEvent(new CustomEvent('focus-meetings-search')), 0);
              })
            }
          >
            <Search />
            Search meetings
          </CommandItem>
          <CommandItem onSelect={() => runCommand(() => navigate('/settings'))}>
            <Settings />
            Open Settings
          </CommandItem>
          <CommandItem onSelect={() => runCommand(runImportAction)}>
            <Import />
            {importActionLabel}
          </CommandItem>
          <CommandItem onSelect={() => runCommand(toggleTheme)}>
            <SunMoon />
            Toggle theme
          </CommandItem>
          <CommandItem
            onSelect={() =>
              runCommand(async () => {
                const meetingId = currentMeeting?.id === 'intro-call' ? null : currentMeeting?.id;
                if (!meetingId) {
                  toast.error('Open a meeting first');
                  return;
                }
                try {
                  await copyMeetingLink(meetingId);
                  toast.success('Meeting link copied');
                } catch (error) {
                  toast.error('Could not copy meeting link', {
                    description: error instanceof Error ? error.message : String(error),
                  });
                }
              })
            }
          >
            <Link2 />
            Copy meeting link
          </CommandItem>
        </CommandGroup>

        {meetings.length > 0 && (
          <>
            <CommandSeparator />
            <CommandGroup heading="Recent meetings">
              {meetings.slice(0, RECENT_MEETINGS_SHOWN).map((meeting) => (
                <CommandItem
                  key={meeting.id}
                  value={`${meeting.title} ${meeting.id}`}
                  onSelect={() =>
                    runCommand(() => openMeeting(meeting))
                  }
                >
                  <FileText />
                  <span className="truncate">{meeting.title || 'Untitled meeting'}</span>
                </CommandItem>
              ))}
            </CommandGroup>
          </>
        )}
      </CommandList>

      <div className="flex items-center justify-end border-t px-3 py-2">
        <CommandShortcut>⌘ / Ctrl + K</CommandShortcut>
      </div>
    </CommandDialog>
  );
}
