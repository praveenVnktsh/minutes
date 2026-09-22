import React from 'react';
import { AlertCircle, CheckCircle2, Loader2, XCircle } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import type { PermissionRowProps } from '@/types/onboarding';

export function PermissionRow({ icon, title, description, status, isPending = false, onAction }: PermissionRowProps) {
  const isAuthorized = status === 'authorized';
  const isDenied = status === 'denied';
  // This row used to have only two faces: green for authorized, red for everything else. That
  // binary is what let it paint "Access Granted" on evidence a denial also produces (the wire
  // verifier can come back genuinely unable to tell, not just allow/deny) — see FR-02/FR-03 in
  // docs/audits/2026-09-21-first-run-flow.md. 'undetermined' needs its own, honest face: we asked,
  // we checked, and we still don't know, which is neither a grant nor a denial.
  const isUndetermined = status === 'undetermined';
  const isChecking = isPending;

  const getButtonText = () => {
    if (isChecking) return 'Checking...';
    if (isDenied) return 'Open Settings';
    if (isUndetermined) return 'Check Again';
    return 'Enable';
  };

  return (
    <div
      className={cn(
        'flex items-center justify-between rounded-2xl border px-6 py-5',
        'transition-all duration-200',
        isAuthorized
          ? 'border-ink bg-surface-2'
          : isDenied
            ? 'border-red-300 bg-red-50'
            : isUndetermined
              ? 'border-warning bg-warning-subtle'
              : 'bg-surface-raised border-hairline'
      )}
    >
      {/* Left side: Icon + Info */}
      <div className="flex items-center gap-3 flex-1 min-w-0">
        {/* Icon */}
        <div
          className={cn(
            'flex size-10 items-center justify-center rounded-full flex-shrink-0',
            isAuthorized
              ? 'bg-surface-2'
              : isDenied
                ? 'bg-red-100'
                : isUndetermined
                  ? 'bg-warning-subtle'
                  : 'bg-neutral-50'
          )}
        >
          <div
            className={cn(
              isAuthorized ? 'text-ink' : isDenied ? 'text-red-500' : isUndetermined ? 'text-warning' : 'text-neutral-500'
            )}
          >
            {icon}
          </div>
        </div>

        {/* Title + Description */}
        <div className="min-w-0 flex-1">
          <div className="font-medium truncate text-ink">{title}</div>
          <div className="text-sm text-muted-foreground">
            {isAuthorized ? (
              <span className="text-green-600 flex items-center gap-1">
                <CheckCircle2 className="w-3.5 h-3.5" />
                Access Granted
              </span>
            ) : isDenied ? (
              <span className="text-red-500 flex items-center gap-1">
                <XCircle className="w-3.5 h-3.5" />
                Access Denied - Please grant in System Settings
              </span>
            ) : isUndetermined ? (
              <span className="text-warning flex items-center gap-1">
                <AlertCircle className="w-3.5 h-3.5" />
                Couldn&apos;t confirm — play some audio and check again
              </span>
            ) : (
              <span>{description}</span>
            )}
          </div>
        </div>
      </div>

      {/* Right side: Action button or checkmark */}
      <div className="flex items-center gap-2 flex-shrink-0 ml-3">
        {!isAuthorized && (
          <Button
            variant={isDenied ? 'destructive' : isUndetermined ? 'warning' : 'outline'}
            size="sm"
            onClick={onAction}
            disabled={isChecking}
            className="min-w-[100px]"
          >
            {isChecking && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            {getButtonText()}
          </Button>
        )}
        {isAuthorized && (
          <div className="flex size-8 items-center justify-center rounded-full bg-green-100">
            <CheckCircle2 className="w-4 h-4 text-green-600" />
          </div>
        )}
      </div>
    </div>
  );
}
