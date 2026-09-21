import { describe, expect, test } from 'bun:test';
import { shouldSeekFromRowClick } from '../../src/components/VirtualizedTranscriptView';

const plainClick = {
  insideInteractive: false,
  selectedText: '',
  pointerMoved: false,
};

describe('shouldSeekFromRowClick', () => {
  test('a plain click on the row seeks', () => {
    expect(shouldSeekFromRowClick(plainClick)).toBe(true);
  });

  test('a click on a control inside the row does not seek', () => {
    expect(shouldSeekFromRowClick({ ...plainClick, insideInteractive: true })).toBe(false);
  });

  test('a click that ends a text selection does not seek', () => {
    expect(shouldSeekFromRowClick({ ...plainClick, selectedText: 'a quote' })).toBe(false);
  });

  test('whitespace left over from a collapsed selection still seeks', () => {
    expect(shouldSeekFromRowClick({ ...plainClick, selectedText: ' \n ' })).toBe(true);
  });

  test('a drag does not seek', () => {
    expect(shouldSeekFromRowClick({ ...plainClick, pointerMoved: true })).toBe(false);
  });

  test('any combination of the three blocks the seek', () => {
    const combinations = [
      { insideInteractive: true, selectedText: 'quote', pointerMoved: false },
      { insideInteractive: true, selectedText: '', pointerMoved: true },
      { insideInteractive: false, selectedText: 'quote', pointerMoved: true },
      { insideInteractive: true, selectedText: 'quote', pointerMoved: true },
    ];
    for (const options of combinations) {
      expect(shouldSeekFromRowClick(options)).toBe(false);
    }
  });

  test('is side-effect free, so repeated calls agree', () => {
    expect(shouldSeekFromRowClick(plainClick)).toBe(shouldSeekFromRowClick(plainClick));
    expect(plainClick).toEqual({ insideInteractive: false, selectedText: '', pointerMoved: false });
  });
});
