export interface MeetingCatalogEntry {
  id: string;
  title: string;
  created_at?: string;
  pinned?: boolean;
  archived?: boolean;
  debug?: boolean;
}

export interface MeetingSearchResult {
  id: string;
  title: string;
  matchContext: string;
  timestamp: string;
}

export interface MeetingVisibility {
  includeArchived?: boolean;
  debugMode?: boolean;
  pinned?: boolean;
}

export function mergeMeetingCatalog(
  current: readonly MeetingCatalogEntry[],
  incoming: readonly MeetingCatalogEntry[],
): MeetingCatalogEntry[] {
  const currentById = new Map(current.map((meeting) => [meeting.id, meeting]));

  return incoming.map((meeting) => {
    const previous = currentById.get(meeting.id);
    return {
      ...previous,
      ...meeting,
      pinned: meeting.pinned ?? previous?.pinned ?? false,
      archived: meeting.archived ?? previous?.archived ?? false,
      debug: meeting.debug ?? previous?.debug ?? false,
    };
  });
}

export function patchMeeting(
  meetings: readonly MeetingCatalogEntry[],
  meetingId: string,
  patch: Partial<Omit<MeetingCatalogEntry, 'id'>>,
): MeetingCatalogEntry[] {
  return meetings.map((meeting) => meeting.id === meetingId ? { ...meeting, ...patch } : meeting);
}

export function selectMeetings(
  meetings: readonly MeetingCatalogEntry[],
  visibility: MeetingVisibility = {},
): MeetingCatalogEntry[] {
  const { includeArchived = false, debugMode = false, pinned } = visibility;
  return meetings.filter((meeting) => {
    if (!debugMode && meeting.debug) return false;
    if (!includeArchived && meeting.archived) return false;
    if (pinned !== undefined && Boolean(meeting.pinned) !== pinned) return false;
    return true;
  });
}

export function selectSearchResults(
  results: readonly MeetingSearchResult[],
  meetings: readonly MeetingCatalogEntry[],
  visibility: MeetingVisibility = {},
): MeetingCatalogEntry[] {
  const visibleById = new Map(
    selectMeetings(meetings, visibility).map((meeting) => [meeting.id, meeting]),
  );
  const seen = new Set<string>();

  return results.flatMap((result) => {
    const meeting = visibleById.get(result.id);
    if (!meeting || seen.has(meeting.id)) return [];
    seen.add(meeting.id);
    return [meeting];
  });
}

export function meetingUrl(meetingId: string): string {
  return `/meeting-details?id=${encodeURIComponent(meetingId)}`;
}

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error || 'Unknown error');
}
