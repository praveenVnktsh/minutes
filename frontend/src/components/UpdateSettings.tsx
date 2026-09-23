'use client';

import { useEffect, useState } from 'react';
import { getVersion } from '@tauri-apps/api/app';
import { RefreshCw } from 'lucide-react';
import { Button } from './ui/button';
import { useUpdateCheckContext } from './UpdateCheckProvider';

/**
 * Settings section for manually checking for app updates.
 */
export function UpdateSettings() {
  const { updateInfo, isChecking, checkError, checkForUpdates, showUpdateDialog } = useUpdateCheckContext();
  const [version, setVersion] = useState('');

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => {});
  }, []);

  const updateAvailable = Boolean(updateInfo?.available);

  return (
    <div className="bg-surface-raised rounded-lg border border-hairline p-6 shadow-sm">
      <h3 className="text-lg font-semibold text-ink mb-2">Updates</h3>
      <p className="text-sm text-ink-muted mb-4">
        You are running Minutes {version || '…'}.
      </p>

      {updateAvailable && updateInfo ? (
        <div className="flex items-center justify-between gap-4">
          <span className="text-sm text-ink">
            Version {updateInfo.version} is available.
          </span>
          <Button size="sm" onClick={showUpdateDialog}>
            Install update
          </Button>
        </div>
      ) : (
        <div className="flex items-center justify-between gap-4">
          <span className="text-sm text-ink-muted">
            {isChecking
              ? ''
              : checkError
                ? "Couldn't check for updates. Try again later."
                : updateInfo
                  ? 'You are up to date.'
                  : ''}
          </span>
          <Button
            size="sm"
            variant="outline"
            onClick={() => void checkForUpdates(true)}
            disabled={isChecking}
          >
            <RefreshCw className={`mr-1.5 h-3.5 w-3.5 ${isChecking ? 'animate-spin' : ''}`} />
            {isChecking ? 'Checking…' : 'Check for updates'}
          </Button>
        </div>
      )}
    </div>
  );
}
