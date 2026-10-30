import { afterEach, describe, expect, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

const { useModalState } = await import('../../src/hooks/useModalState');
type ModalState = ReturnType<typeof useModalState>;

let current!: ModalState;
let renderer: ReactTestRenderer | undefined;

function Probe() {
  current = useModalState();
  return null;
}

afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

describe('useModalState', () => {
  test('shows a modal with its message and clears the message on hide', async () => {
    await act(async () => { renderer = create(<Probe />); });
    act(() => current.showModal('errorAlert', 'Microphone unavailable'));
    expect(current.modals.errorAlert).toBe(true);
    expect(current.messages.errorAlert).toBe('Microphone unavailable');

    act(() => current.hideModal('errorAlert'));
    expect(current.modals.errorAlert).toBe(false);
    expect(current.messages.errorAlert).toBe('');
  });

  test('hides every modal at once', async () => {
    await act(async () => { renderer = create(<Probe />); });
    act(() => {
      current.showModal('deviceSettings');
      current.showModal('modelSelector', 'Download the speech models first');
    });
    act(() => current.hideAllModals());
    expect(Object.values(current.modals).every(open => !open)).toBe(true);
    expect(current.messages.modelSelector).toBe('');
  });
});
