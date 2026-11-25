'use client';

import { Switch } from '@/components/ui/switch';
import { useShell } from '@/contexts/ShellContext';

/** Settings → General: light or dark theme (the command palette can also toggle it). */
export function AppearanceSetting() {
  const { theme, setTheme } = useShell();
  return (
    <div className="bg-surface-raised rounded-lg border border-hairline p-6 shadow-sm">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h3 className="text-lg font-semibold text-ink mb-2">Dark mode</h3>
          <p className="text-sm text-ink-muted">Use the dark theme</p>
        </div>
        <Switch
          aria-label="Use the dark theme"
          checked={theme === 'dark'}
          onCheckedChange={(checked) => setTheme(checked ? 'dark' : 'light')}
        />
      </div>
    </div>
  );
}
