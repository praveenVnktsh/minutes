export type MeetingActivityKind = 'import' | 'recording' | 'retranscription';

export type MeetingActivityStatus =
  | 'starting'
  | 'queued'
  | 'recording'
  | 'paused'
  | 'saving'
  | 'transcribing'
  | 'ready'
  | 'failed'
  | 'cancelled';

export interface RecordingActivity {
  session_id: string;
  meeting_id: string | null;
  status: MeetingActivityStatus;
  error: string | null;
}

export interface RecordingCommandResult {
  session_id: string;
}

export interface MeetingActivity {
  task_id: string;
  meeting_id: string | null;
  kind: MeetingActivityKind;
  status: MeetingActivityStatus;
  title: string;
  stage: string | null;
  progress_percentage: number | null;
  message: string | null;
  error: string | null;
  controls_available: boolean;
  revision: number;
}

export interface MeetingActivitySnapshot {
  revision: number;
  recording: RecordingActivity | null;
  activities: MeetingActivity[];
}

export interface RecordingStartRequest {
  request_id: string;
  source: 'meeting_prompt' | 'tray' | string;
  status: 'pending' | 'claimed' | 'starting';
  claimed_by: string | null;
}

export interface RecordingRequestResult {
  request_id: string;
  accepted: boolean;
  error: string | null;
}
