'use client';

import { Toaster } from 'sonner';
import 'sonner/dist/styles.css';
import { useShell } from '@/contexts/ShellContext';

export function ThemedToaster() {
  const { theme } = useShell();
  return (
    <Toaster
      position="bottom-center"
      richColors
      closeButton
      theme={theme}
      toastOptions={{
        classNames: {
          toast: 'border-hairline bg-surface-raised text-ink shadow-lg',
          description: 'text-ink-muted',
          actionButton: 'bg-brand text-brand-foreground',
          cancelButton: 'bg-surface-2 text-ink',
          closeButton: 'border-hairline bg-surface-raised text-ink',
        },
      }}
    />
  );
}
