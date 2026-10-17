'use client';

import { useEffect, useState } from 'react';
import { CitedMarkdown } from '@/components/MeetingDetails/CitedMarkdown';
import { useLiveSource } from '@/lib/liveSource';
import {
  currentLiveEnhanced,
  LIVE_ENHANCED_UPDATE_EVENT,
  type LiveEnhancedSnapshot,
} from '@/lib/liveEnhanced';

/**
 * The Enhanced notes while the meeting records: each topic's section appears
 * once the conversation moves on from it, and the topic under way shows a
 * quiet "Writing…". Read-only; after the meeting the full notes replace it.
 */
export function LiveEnhancedNotes() {
  const source = useLiveSource();
  const [snapshot, setSnapshot] = useState<LiveEnhancedSnapshot | null>(null);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void currentLiveEnhanced(source)
      .then((current) => {
        if (!cancelled && current) setSnapshot((previous) => previous ?? current);
      })
      .catch(() => {});
    void source
      .listen<LiveEnhancedSnapshot>(LIVE_ENHANCED_UPDATE_EVENT, (event) => {
        if (!cancelled) setSnapshot(event.payload);
      })
      .then((stop) => {
        if (cancelled) stop();
        else unlisten = stop;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [source]);

  const sections = snapshot?.sections ?? [];
  const tail = snapshot?.tail.trim() ?? '';

  return (
    <div className="h-full overflow-y-auto" data-testid="live-enhanced-notes">
      <div className="mx-auto w-full max-w-[860px] px-10 pb-24 pt-6">
        {sections.length === 0 && !tail ? (
          <p className="pt-10 text-center text-sm text-ink-subtle">
            Notes appear here as the meeting moves from topic to topic.
          </p>
        ) : (
          <div className="text-sm leading-6 text-ink [&_h2]:mb-2 [&_h2]:mt-6 [&_h2]:font-serif [&_h2]:text-lg [&_h2]:font-semibold [&_h2]:text-ink [&_li]:my-1 [&_p]:my-2 [&_ul]:list-disc [&_ul]:pl-5">
            {sections.map((section, index) => (
              <section key={`${section.title}-${index}`}>
                <h2>{section.title}</h2>
                {section.body.trim() && <CitedMarkdown>{section.body}</CitedMarkdown>}
                {section.writing && (
                  <p className="text-xs text-ink-subtle" aria-live="polite">Writing…</p>
                )}
              </section>
            ))}
            {tail && <CitedMarkdown>{tail}</CitedMarkdown>}
          </div>
        )}
      </div>
    </div>
  );
}
