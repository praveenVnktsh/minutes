'use client';

import { useEffect, useState, type FormEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Loader2, Send } from 'lucide-react';
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
import { buildFeedbackIssueUrl, collectFeedbackContext } from '@/lib/feedback';

interface SendFeedbackDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function SendFeedbackDialog({ open, onOpenChange }: SendFeedbackDialogProps) {
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
      const context = await collectFeedbackContext();
      const url = buildFeedbackIssueUrl({ title, description }, context);
      await invoke('open_external_url', { url });
      onOpenChange(false);
    } catch (error) {
      console.error('Failed to open the feedback form:', error);
      setIsSubmitting(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
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
