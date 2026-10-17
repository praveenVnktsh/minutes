import { useSyncExternalStore } from 'react';

/** False only when the document reports itself hidden (window minimised, on
 * another Space, closed to the menu bar). */
export function isPageVisible(doc: Pick<Document, 'visibilityState'> | undefined): boolean {
  return doc?.visibilityState !== 'hidden';
}

function currentDocument(): Document | undefined {
  return typeof document === 'undefined' ? undefined : document;
}

function subscribe(onChange: () => void): () => void {
  if (typeof document === 'undefined') return () => {};
  document.addEventListener('visibilitychange', onChange);
  return () => document.removeEventListener('visibilitychange', onChange);
}

/** Whether anyone can see the page; use it to stop polling and animation
 * work while the window is hidden. */
export function usePageVisible(): boolean {
  return useSyncExternalStore(
    subscribe,
    () => isPageVisible(currentDocument()),
    () => true,
  );
}
