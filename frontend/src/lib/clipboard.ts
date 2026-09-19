export async function writeClipboardText(text: string): Promise<void> {
  if (typeof navigator === 'undefined' || !navigator.clipboard?.writeText) {
    throw new Error('Clipboard access is unavailable');
  }

  await navigator.clipboard.writeText(text);
}

export function meetingDeepLink(meetingId: string): string {
  return `minutes://meeting/${encodeURIComponent(meetingId)}`;
}

export async function copyMeetingLink(meetingId: string): Promise<void> {
  await writeClipboardText(meetingDeepLink(meetingId));
}
