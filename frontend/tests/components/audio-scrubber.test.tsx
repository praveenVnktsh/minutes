import { describe, expect, test } from 'bun:test';
import { createElement } from 'react';
import { act, create } from 'react-test-renderer';
import { renderToStaticMarkup } from 'react-dom/server';
import {
  AudioScrubber,
  formatClock,
  timeFromClientX,
} from '../../src/components/MeetingDetails/AudioScrubber';

const rect = {
  left: 0,
  width: 100,
  right: 100,
  top: 0,
  bottom: 6,
  height: 6,
  x: 0,
  y: 0,
  toJSON() { return this; },
};

function pointer(clientX: number) {
  return {
    pointerId: 1,
    clientX,
    preventDefault() {},
    currentTarget: {
      getBoundingClientRect: () => rect,
      setPointerCapture() {},
    },
  };
}

describe('AudioScrubber', () => {
  test('maps a pointer position onto the recording clock', () => {
    expect(timeFromClientX(50, { left: 0, width: 100 }, 20)).toBe(10);
    expect(timeFromClientX(-10, { left: 0, width: 100 }, 20)).toBe(0);
    expect(timeFromClientX(200, { left: 0, width: 100 }, 20)).toBe(20);
    expect(timeFromClientX(50, { left: 0, width: 0 }, 20)).toBe(0);
    expect(timeFromClientX(50, { left: 0, width: 100 }, 0)).toBe(0);
  });

  test('formats the clock as mm:ss', () => {
    expect(formatClock(0)).toBe('00:00');
    expect(formatClock(65)).toBe('01:05');
    expect(formatClock(-1)).toBe('00:00');
    expect(formatClock(Number.NaN)).toBe('00:00');
  });

  test('renders a seek slider and play control', () => {
    const html = renderToStaticMarkup(
      createElement(AudioScrubber, {
        currentTime: 12,
        duration: 120,
        isPlaying: false,
        onTogglePlayback: () => {},
        onScrubPreview: () => {},
        onScrubCommit: () => {},
      }),
    );
    expect(html).toContain('role="slider"');
    expect(html).toContain('Play recording');
    expect(html).toContain('00:12');
    expect(html).toContain('02:00');
  });

  test('emits preview while dragging and commit on release', () => {
    const previews: number[] = [];
    const commits: number[] = [];
    const renderer = create(
      createElement(AudioScrubber, {
        currentTime: 0,
        duration: 100,
        isPlaying: false,
        onTogglePlayback: () => {},
        onScrubPreview: (time: number) => previews.push(time),
        onScrubCommit: (time: number) => commits.push(time),
      }),
    );
    const slider = renderer.root.findByProps({ role: 'slider' });
    act(() => {
      slider.props.onPointerDown(pointer(25));
      slider.props.onPointerMove(pointer(40));
      slider.props.onPointerUp(pointer(40));
    });
    expect(previews).toEqual([25, 40]);
    expect(commits).toEqual([40]);
    renderer.unmount();
  });
});
