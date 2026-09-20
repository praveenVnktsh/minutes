'use client';

import { Toaster } from 'sonner';
import type { ToasterProps } from 'sonner';
import type { CSSProperties } from 'react';
import 'sonner/dist/styles.css';
import { useShell } from '@/contexts/ShellContext';

export const THEMED_TOAST_VARIABLES = {
  '--normal-bg': 'var(--surface-raised)',
  '--normal-bg-hover': 'var(--surface-2)',
  '--normal-border': 'var(--hairline)',
  '--normal-border-hover': 'hsl(var(--input))',
  '--normal-text': 'var(--ink)',
  '--success-bg': 'var(--success-subtle)',
  '--success-border': 'var(--success)',
  '--success-text': 'var(--success)',
  '--info-bg': 'var(--info-subtle)',
  '--info-border': 'var(--info)',
  '--info-text': 'var(--info)',
  '--warning-bg': 'var(--warning-subtle)',
  '--warning-border': 'var(--warning)',
  '--warning-text': 'var(--warning)',
  '--error-bg': 'var(--error-subtle)',
  '--error-border': 'var(--error)',
  '--error-text': 'var(--error)',
} as const;

export const SHARED_TOASTER_PROPS: Omit<ToasterProps, 'theme'> = {
  position: 'bottom-center',
  richColors: true,
  closeButton: true,
  className: 'minutes-toaster',
  style: THEMED_TOAST_VARIABLES as CSSProperties,
};

export function ThemedToaster() {
  const { theme } = useShell();
  return <Toaster theme={theme} {...SHARED_TOASTER_PROPS} />;
}
