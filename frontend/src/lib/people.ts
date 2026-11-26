import { invoke } from '@tauri-apps/api/core';

/**
 * The local people directory and meeting attendees (Rust: `src/people`).
 * People come from Linear, Notion, calendar invites or the user; nothing
 * about them is sent anywhere. See
 * docs/superpowers/specs/2026-11-26-attendee-context-design.md.
 */

export type PeopleSource = 'linear' | 'notion' | 'calendar' | 'manual';

export interface PersonProject {
  title: string;
  url?: string;
  source: PeopleSource;
}

export interface Person {
  id: string;
  name: string;
  email: string | null;
  role: string | null;
  team: string | null;
  aliases: string[];
  projects: PersonProject[];
  notes: string | null;
  is_me: boolean;
  /** Fields the user edited by hand; syncs leave them alone. */
  locked_fields: string[];
  sources: { source: PeopleSource; external_id: string }[];
}

/** `undefined` leaves a field alone; `''` clears it. Set fields are locked. */
export interface PersonEdit {
  name?: string;
  email?: string;
  role?: string;
  team?: string;
  aliases?: string[];
  notes?: string;
  is_me?: boolean;
}

export interface MeetingPeopleContext {
  attendees: Person[];
  me: Person | null;
  vocabulary: string[];
}

export const peopleApi = {
  list: () => invoke<Person[]>('people_list'),
  create: (edit: PersonEdit) => invoke<Person>('people_create', { edit }),
  update: (personId: string, edit: PersonEdit) => invoke<Person>('people_update', { personId, edit }),
  remove: (personId: string) => invoke<void>('people_delete', { personId }),
  attendees: (meetingId: string) => invoke<Person[]>('meeting_attendees_get', { meetingId }),
  addAttendee: (meetingId: string, personId: string) =>
    invoke<Person[]>('meeting_attendees_add', { meetingId, personId }),
  removeAttendee: (meetingId: string, personId: string) =>
    invoke<Person[]>('meeting_attendees_remove', { meetingId, personId }),
  context: (meetingId: string) => invoke<MeetingPeopleContext>('meeting_people_context', { meetingId }),
};
