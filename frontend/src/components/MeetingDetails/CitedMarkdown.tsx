'use client';

import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { citationHrefSeconds, jumpToSeconds, linkifyCitations } from '@/lib/copilot';

/**
 * Markdown where `[MM:SS]` citations are buttons that scroll the transcript to
 * that moment. Times, not row ids, so they still land correctly after the
 * final transcript replaced the live one.
 */
export function CitedMarkdown({ children }: { children: string }) {
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      urlTransform={(url) => url}
      components={{
        a: ({ href, children: label }) => {
          const seconds = citationHrefSeconds(href);
          if (seconds === null) {
            return <a href={href} target="_blank" rel="noreferrer">{label}</a>;
          }
          return (
            <button
              type="button"
              onClick={() => jumpToSeconds(seconds)}
              title="Show this moment in the transcript"
              className="rounded px-0.5 font-mono text-[11px] tabular-nums text-ink underline decoration-hairline underline-offset-2 hover:bg-surface-2"
            >
              {label}
            </button>
          );
        },
      }}
    >
      {linkifyCitations(children)}
    </ReactMarkdown>
  );
}
