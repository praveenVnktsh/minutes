import type { LiveSource } from '@/lib/liveSource';

/** One heading of the Enhanced notes while the meeting records. */
export interface LiveEnhancedSection {
  title: string;
  /** Markdown bullets written so far; empty while the section is written. */
  body: string;
  /** The topic under way, or a finished topic whose section is on its way. */
  writing: boolean;
}

/** `live-enhanced-update` payload and `get_live_enhanced` result. */
export interface LiveEnhancedSnapshot {
  session_id: string;
  meeting_id: string | null;
  sections: LiveEnhancedSection[];
  /** "Also discussed", decisions, action items and open questions so far. */
  tail: string;
}

export const LIVE_ENHANCED_UPDATE_EVENT = 'live-enhanced-update';

/** The recording's Enhanced notes so far, through a live source. */
export function currentLiveEnhanced(source: LiveSource): Promise<LiveEnhancedSnapshot | null> {
  return source.invoke<LiveEnhancedSnapshot | null>('get_live_enhanced');
}
