import React, { useState, useEffect } from "react";
import { getVersion } from '@tauri-apps/api/app';
import Image from 'next/image';
import { UpdateDialog } from "./UpdateDialog";
import { updateService, UpdateInfo } from '@/services/updateService';
import { Button } from './ui/button';
import { Loader2, CheckCircle2 } from 'lucide-react';
import { toast } from 'sonner';
import { toneText } from '@/lib/theme-classes';


export function About() {
    const [currentVersion, setCurrentVersion] = useState<string>('0.6.0');
    const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
    const [isChecking, setIsChecking] = useState(false);
    const [showUpdateDialog, setShowUpdateDialog] = useState(false);

    useEffect(() => {
        // Get current version on mount
        getVersion().then(setCurrentVersion).catch(console.error);
    }, []);

    const handleCheckForUpdates = async () => {
        setIsChecking(true);
        try {
            const info = await updateService.checkForUpdates(true);
            setUpdateInfo(info);
            if (info.available) {
                setShowUpdateDialog(true);
            } else {
                toast.success('You are running the latest version');
            }
        } catch (error: any) {
            console.error('Failed to check for updates:', error);
            toast.error('Failed to check for updates: ' + (error.message || 'Unknown error'));
        } finally {
            setIsChecking(false);
        }
    };

    return (
        <div className="p-4 space-y-4 h-[80vh] overflow-y-auto">
            {/* Compact Header */}
            <div className="text-center">
                <div className="mb-3">
                    <Image
                        src="icon_128x128.png"
                        alt="Minutes Logo"
                        width={64}
                        height={64}
                        className="mx-auto"
                    />
                </div>
                {/* <h1 className="text-xl font-bold text-ink">Minutes</h1> */}
                <span className="text-sm text-ink-muted"> v{currentVersion}</span>
                <p className="text-medium text-ink-muted mt-1">
                    Real-time notes and summaries that never leave your machine.
                </p>
                <div className="mt-3">
                    <Button
                        onClick={handleCheckForUpdates}
                        disabled={isChecking}
                        variant="outline"
                        size="sm"
                        className="text-xs"
                    >
                        {isChecking ? (
                            <>
                                <Loader2 className="h-3 w-3 mr-2 animate-spin" />
                                Checking...
                            </>
                        ) : (
                            <>
                                <CheckCircle2 className="h-3 w-3 mr-2" />
                                Check for Updates
                            </>
                        )}
                    </Button>
                    {updateInfo?.available && (
                        <div className={`mt-2 text-xs ${toneText.info}`}>
                            Update available: v{updateInfo.version}
                        </div>
                    )}
                </div>
            </div>

            {/* Features Grid - Compact */}
            <div className="space-y-3">
                <h2 className="text-base font-semibold text-ink">What makes Minutes different</h2>
                <div className="grid grid-cols-2 gap-2">
                    <div className="bg-surface-2 rounded p-3 hover:bg-surface-2 transition-colors">
                        <h3 className="font-bold text-sm text-ink mb-1">Privacy-first</h3>
                        <p className="text-xs text-ink-muted leading-relaxed">Your data & AI processing workflow can now stay within your premise. No cloud, no leaks.</p>
                    </div>
                    <div className="bg-surface-2 rounded p-3 hover:bg-surface-2 transition-colors">
                        <h3 className="font-bold text-sm text-ink mb-1">Use Any Model</h3>
                        <p className="text-xs text-ink-muted leading-relaxed">Prefer local open-source model? Great. Want to plug in an external API? Also fine. No lock-in.</p>
                    </div>
                    <div className="bg-surface-2 rounded p-3 hover:bg-surface-2 transition-colors">
                        <h3 className="font-bold text-sm text-ink mb-1">Cost-Smart</h3>
                        <p className="text-xs text-ink-muted leading-relaxed">Avoid pay-per-minute bills by running models locally (or pay only for the calls you choose).</p>
                    </div>
                    <div className="bg-surface-2 rounded p-3 hover:bg-surface-2 transition-colors">
                        <h3 className="font-bold text-sm text-ink mb-1">Works everywhere</h3>
                        <p className="text-xs text-ink-muted leading-relaxed">Google Meet, Zoom, Teams-online or offline.</p>
                    </div>
                </div>
            </div>

            {/* Footer - Compact */}
            <div className="pt-2 border-t border-hairline text-center">
                <p className="text-xs text-ink-subtle">
                    Open-source software licensed under MIT
                </p>
            </div>

            {/* Update Dialog */}
            <UpdateDialog
                open={showUpdateDialog}
                onOpenChange={setShowUpdateDialog}
                updateInfo={updateInfo}
            />
        </div>

    )
}
