import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import type {
  MeetingActivitySnapshot,
  RecordingStartRequest,
  RecordingRequestResult,
} from '@/types/meetingActivity';

const ACTIVITY_EVENT = 'meeting-activity-updated';

export class MeetingActivityService {
  getSnapshot(): Promise<MeetingActivitySnapshot> {
    return invoke<MeetingActivitySnapshot>('get_meeting_activity_snapshot');
  }

  bindActiveRecordingMeeting(
    sessionId: string,
    meetingId: string
  ): Promise<MeetingActivitySnapshot> {
    return invoke<MeetingActivitySnapshot>('bind_active_recording_meeting', {
      sessionId,
      meetingId,
    });
  }

  getPendingRecordingRequest(): Promise<RecordingStartRequest | null> {
    return invoke<RecordingStartRequest | null>('get_pending_recording_request');
  }

  claimRecordingRequest(
    requestId: string,
    claimant: string
  ): Promise<RecordingStartRequest> {
    return invoke<RecordingStartRequest>('claim_recording_request', {
      requestId,
      claimant,
    });
  }

  async subscribe(
    callback: (snapshot: MeetingActivitySnapshot) => void
  ): Promise<UnlistenFn> {
    let latestRevision = -1;
    const deliver = (snapshot: MeetingActivitySnapshot) => {
      if (snapshot.revision > latestRevision) {
        latestRevision = snapshot.revision;
        callback(snapshot);
      }
    };

    // Listen first so an update cannot be lost between hydration and subscription.
    const unlisten = await listen<MeetingActivitySnapshot>(ACTIVITY_EVENT, (event) => {
      deliver(event.payload);
    });

    try {
      deliver(await this.getSnapshot());
      return unlisten;
    } catch (error) {
      unlisten();
      throw error;
    }
  }

  onRecordingStartRequested(
    callback: (request: RecordingStartRequest) => void
  ): Promise<UnlistenFn> {
    return listen<RecordingStartRequest>('recording-start-requested', (event) => {
      callback(event.payload);
    });
  }

  onPromptStartResult(
    callback: (result: RecordingRequestResult) => void
  ): Promise<UnlistenFn> {
    return listen<RecordingRequestResult>('meeting-prompt-start-result', (event) => {
      callback(event.payload);
    });
  }

  acknowledgeRecordingRequest(
    requestId: string,
    accepted: boolean,
    error: string | null = null
  ): Promise<void> {
    return invoke('acknowledge_recording_request', {
      requestId,
      accepted,
      error,
    });
  }
}

export const meetingActivityService = new MeetingActivityService();
