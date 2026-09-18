import { describe, expect, test } from 'bun:test';
import {
  isActiveSegmentJump,
  isScrollbarPointer,
  isUserScrollKey,
  shouldFollowActiveSegment,
} from '../../src/hooks/useAutoScroll';

describe('useAutoScroll follow', () => {
  test('treats the first segment and a skipped index as a jump', () => {
    expect(isActiveSegmentJump(-1, 0)).toBe(true);
    expect(isActiveSegmentJump(2, 8)).toBe(true);
    expect(isActiveSegmentJump(4, 5)).toBe(false);
    expect(isActiveSegmentJump(5, 4)).toBe(false);
    expect(isActiveSegmentJump(3, -1)).toBe(false);
  });

  test('follows sequential playback only while follow is enabled', () => {
    expect(shouldFollowActiveSegment(4, 5, true)).toBe(true);
    expect(shouldFollowActiveSegment(4, 5, false)).toBe(false);
  });

  test('resumes follow after a scrub jump even if the user had scrolled', () => {
    expect(shouldFollowActiveSegment(1, 9, false)).toBe(true);
    expect(shouldFollowActiveSegment(-1, 0, false)).toBe(true);
  });

  test('treats wheel, page, and arrow keys as user scroll intent', () => {
    expect(isUserScrollKey('ArrowDown')).toBe(true);
    expect(isUserScrollKey('PageUp')).toBe(true);
    expect(isUserScrollKey(' ')).toBe(true);
    expect(isUserScrollKey('Enter')).toBe(false);
  });

  test('detects a pointer on the scrollbar gutter, not the content', () => {
    const container = {
      offsetWidth: 320,
      clientWidth: 300,
      offsetHeight: 400,
      clientHeight: 400,
      getBoundingClientRect: () => ({ right: 320, bottom: 400 }),
    } as HTMLElement;
    expect(isScrollbarPointer({ clientX: 310, clientY: 40 }, container)).toBe(true);
    expect(isScrollbarPointer({ clientX: 40, clientY: 40 }, container)).toBe(false);
  });
});
