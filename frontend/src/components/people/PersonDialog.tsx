'use client';

import { useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { Textarea } from '@/components/ui/textarea';
import type { Person, PersonEdit } from '@/lib/people';

interface Draft {
  name: string;
  email: string;
  role: string;
  team: string;
  aliases: string;
  notes: string;
  is_me: boolean;
}

const EMPTY: Draft = { name: '', email: '', role: '', team: '', aliases: '', notes: '', is_me: false };

function draftOf(person: Person | null): Draft {
  if (!person) return EMPTY;
  return {
    name: person.name,
    email: person.email ?? '',
    role: person.role ?? '',
    team: person.team ?? '',
    aliases: person.aliases.join(', '),
    notes: person.notes ?? '',
    is_me: person.is_me,
  };
}

export function parseAliases(text: string): string[] {
  return text.split(',').map(alias => alias.trim()).filter(Boolean);
}

/**
 * Only the fields that changed. Every field sent is locked against later
 * syncs, so an untouched field must stay out of the edit.
 */
export function editFrom(before: Draft, after: Draft): PersonEdit {
  const edit: PersonEdit = {};
  for (const field of ['name', 'email', 'role', 'team', 'notes'] as const) {
    if (after[field].trim() !== before[field].trim()) edit[field] = after[field].trim();
  }
  const aliases = parseAliases(after.aliases);
  if (aliases.join('\n') !== parseAliases(before.aliases).join('\n')) edit.aliases = aliases;
  if (after.is_me !== before.is_me) edit.is_me = after.is_me;
  return edit;
}

interface PersonDialogProps {
  open: boolean;
  /** `null` adds a new person. */
  person: Person | null;
  onOpenChange: (open: boolean) => void;
  onSave: (edit: PersonEdit) => Promise<boolean>;
}

const FIELDS = [
  { key: 'name', label: 'Name', placeholder: 'Priya Shah' },
  { key: 'email', label: 'Email', placeholder: 'priya@example.com' },
  { key: 'role', label: 'Role', placeholder: 'Product manager' },
  { key: 'team', label: 'Team', placeholder: 'Growth' },
  { key: 'aliases', label: 'Aliases (comma-separated)', placeholder: 'Pri, PS' },
] as const;

export function PersonDialog({ open, person, onOpenChange, onSave }: PersonDialogProps) {
  const [draft, setDraft] = useState<Draft>(() => draftOf(person));
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (open) setDraft(draftOf(person));
  }, [open, person]);

  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => setDraft(current => ({ ...current, [key]: value }));

  const save = async () => {
    setSaving(true);
    try {
      const edit = editFrom(person ? draftOf(person) : EMPTY, draft);
      if (await onSave(edit)) onOpenChange(false);
    } finally {
      setSaving(false);
    }
  };

  const locked = person?.locked_fields ?? [];

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{person ? `Edit ${person.name}` : 'Add a person'}</DialogTitle>
          <DialogDescription>
            Fields you change here stay as you set them; later Linear or Notion syncs leave them alone.
          </DialogDescription>
        </DialogHeader>
        <form
          aria-label="Person details"
          className="space-y-3"
          onSubmit={event => { event.preventDefault(); void save(); }}
        >
          {FIELDS.map(field => (
            <div key={field.key} className="space-y-1">
              <label htmlFor={`person-${field.key}`} className="text-sm font-medium">
                {field.label}
                {locked.includes(field.key) && <span className="ml-2 text-xs font-normal text-ink-muted">edited by you</span>}
              </label>
              <Input
                id={`person-${field.key}`}
                aria-label={field.label}
                value={draft[field.key]}
                placeholder={field.placeholder}
                onChange={event => set(field.key, event.target.value)}
              />
            </div>
          ))}
          <div className="space-y-1">
            <label htmlFor="person-notes" className="text-sm font-medium">Notes</label>
            <Textarea
              id="person-notes"
              aria-label="Notes"
              rows={3}
              value={draft.notes}
              onChange={event => set('notes', event.target.value)}
            />
          </div>
          <div className="flex items-center justify-between rounded-lg border border-hairline bg-surface-raised p-3">
            <div className="pr-4">
              <div className="text-sm font-medium">This is me</div>
              <div className="text-xs text-ink-muted">Notes and action items name you as the user.</div>
            </div>
            <Switch aria-label="This is me" checked={draft.is_me} onCheckedChange={value => set('is_me', value)} />
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={saving}>Cancel</Button>
            <Button type="submit" disabled={saving || !draft.name.trim()}>
              {saving ? 'Saving…' : person ? 'Save' : 'Add person'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
