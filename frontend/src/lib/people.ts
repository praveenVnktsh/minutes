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

/**
 * Linear and Notion syncs (Rust: `src/people/sync.rs`). Keys and tokens go in
 * and are never returned; they are sent only to their own source's API.
 */
export type PeopleSyncSource = 'linear' | 'notion';

/** Emitted with the new `PeopleSourceStatus[]` after a sync or a change. */
export const PEOPLE_SYNC_EVENT = 'people-sync-updated';

export type NotionField = 'name' | 'email' | 'role' | 'team' | 'projects';

/** Which Notion property feeds each field; `null` when unmapped. */
export type NotionMapping = Record<NotionField, string | null>;

/** Per field: `null`/absent uses the detected property, `''` maps nothing, a name picks that property. */
export type NotionOverrides = Partial<Record<NotionField, string | null>>;

export interface NotionConfig {
  database_id: string | null;
  database_title: string | null;
  mapping: NotionOverrides;
}

export interface PeopleSourceStatus {
  source: PeopleSyncSource;
  connected: boolean;
  workspace: string | null;
  /** Last successful sync (RFC 3339). */
  last_synced_at: string | null;
  last_attempt_at: string | null;
  last_error: string | null;
  last_count: number | null;
  notion: NotionConfig | null;
}

export interface PeopleConnectionTest {
  workspace: string | null;
  /** Linear: active users. Notion: rows in the chosen database. */
  user_count: number | null;
}

export interface NotionDatabase {
  id: string;
  title: string;
  url: string | null;
  properties: { name: string; type: string }[];
  detected: NotionMapping;
}

export interface PeopleSyncResult {
  source: PeopleSyncSource;
  synced: number;
  failed: number;
  error: string | null;
}

export const peopleSyncApi = {
  status: () => invoke<PeopleSourceStatus[]>('people_sync_status'),
  connect: (source: PeopleSyncSource, token: string) =>
    invoke<{ test: PeopleConnectionTest; statuses: PeopleSourceStatus[] }>('people_connect', { source, token }),
  test: (source: PeopleSyncSource, token?: string) =>
    invoke<PeopleConnectionTest>('people_test_connection', { source, token: token ?? null }),
  notionDatabases: () => invoke<NotionDatabase[]>('people_notion_databases'),
  saveNotionMapping: (databaseId: string, databaseTitle: string | null, mapping: NotionOverrides) =>
    invoke<PeopleSourceStatus[]>('people_notion_save_mapping', { databaseId, databaseTitle, mapping }),
  syncNow: (source?: PeopleSyncSource) => invoke<PeopleSyncResult[]>('people_sync_now', { source: source ?? null }),
  disconnect: (source: PeopleSyncSource) => invoke<number>('people_disconnect', { source }),
};
