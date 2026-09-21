'use client';

import { useEffect, useState, type FormEvent, type RefObject } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Loader2, Send } from 'lucide-react';
import { toast } from 'sonner';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Textarea } from '@/components/ui/textarea';
import { writeClipboardText } from '@/lib/clipboard';
import {
  buildFeedbackClipboardText,
  buildFeedbackIssueUrl,
  collectFeedbackContext,
  feedbackIssuesAreOpen,
} from '@/lib/feedback';

interface SendFeedbackDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  returnFocusRef?: RefObject<HTMLElement>;
}

export function SendFeedbackDialog({ open, onOpenChange, returnFocusRef }: SendFeedbackDialogProps) {
  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [isSubmitting, setIsSubmitting] = useState(false);

  // Start each visit with a clean form.
  useEffect(() => {
    if (open) {
      setTitle('');
      setDescription('');
      setIsSubmitting(false);
    }
  }, [open]);

  const handleSubmit = async (event: FormEvent) => {
    event.preventDefault();
    if (!description.trim()) return;

    setIsSubmitting(true);
    try {
      const draft = { title, description };
      const context = await collectFeedbackContext();

      if (await feedbackIssuesAreOpen()) {
        const url = buildFeedbackIssueUrl(draft, context);
        await invoke('open_external_url', { url });
        onOpenChange(false);
        return;
      }

      // Issues are closed on the project repo: opening the URL would just be a
      // 404, so hand the user their draft instead of losing it.
      try {
        await writeClipboardText(buildFeedbackClipboardText(draft, context));
        toast.warning('Issue reporting is closed on the project repo', {
          description: 'Your feedback was copied to the clipboard so you can send it elsewhere.',
        });
        onOpenChange(false);
      } catch (clipboardError) {
        console.error('Failed to copy feedback to the clipboard:', clipboardError);
        toast.error('Could not copy feedback to the clipboard', {
          description: 'Issue reporting is closed on the project repo. Please try again.',
        });
        setIsSubmitting(false);
      }
    } catch (error) {
      console.error('Failed to open the feedback form:', error);
      setIsSubmitting(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="max-w-lg"
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          if (returnFocusRef?.current?.isConnected) returnFocusRef.current.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle>Send feedback</DialogTitle>
          <DialogDescription>
            Tell us what is working, what is not, or what you would like to see next. This opens a
            prefilled issue on GitHub for you to review before submitting.
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="space-y-2">
            <label htmlFor="feedback-title" className="text-sm font-medium">
              Title
            </label>
            <Input
              id="feedback-title"
              value={title}
              onChange={(event) => setTitle(event.target.value)}
              placeholder="A short summary"
              autoFocus
            />
          </div>

          <div className="space-y-2">
            <label htmlFor="feedback-description" className="text-sm font-medium">
              Description
            </label>
            <Textarea
              id="feedback-description"
              value={description}
              onChange={(event) => setDescription(event.target.value)}
              placeholder="What happened, what you expected, and any steps to reproduce it."
              rows={6}
            />
          </div>

          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={!description.trim() || isSubmitting}>
              {isSubmitting ? (
                <>
                  <Loader2 className="animate-spin" />
                  Opening…
                </>
              ) : (
                <>
                  <Send />
                  Create issue
                </>
              )}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
