'use client';

import { DeviceSelection } from '@/components/DeviceSelection';
import { LanguageSelection } from '@/components/LanguageSelection';
import { ModelConfigForm } from '@/components/settings/ModelConfigForm';
import { TranscriptSettings } from '@/components/TranscriptSettings';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Switch } from '@/components/ui/switch';
import { useConfig } from '@/contexts/ConfigContext';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { toast } from 'sonner';

type ModalType = 'modelSettings' | 'deviceSettings' | 'languageSettings' | 'modelSelector' | 'errorAlert' | 'chunkDropWarning';

interface SettingsModalsProps {
  modals: Record<ModalType, boolean>;
  messages: Pick<Record<ModalType, string>, 'errorAlert' | 'chunkDropWarning' | 'modelSelector'>;
  onClose: (name: ModalType) => void;
}

export function SettingsModals({ modals, messages, onClose }: SettingsModalsProps) {
  const {
    selectedDevices,
    setSelectedDevices,
    selectedLanguage,
    setSelectedLanguage,
    transcriptModelConfig,
    setTranscriptModelConfig,
    showConfidenceIndicator,
    toggleConfidenceIndicator,
    isModelConfigSaving,
  } = useConfig();
  const { isRecording } = useRecordingState();

  const closeOnChange = (name: ModalType, open: boolean) => {
    if (!open && !(name === 'modelSettings' && isModelConfigSaving)) onClose(name);
  };

  return (
    <>
      <Dialog open={modals.modelSettings} onOpenChange={open => closeOnChange('modelSettings', open)}>
        <DialogContent
          className="max-h-[90vh] max-w-3xl overflow-y-auto"
          onEscapeKeyDown={event => { if (isModelConfigSaving) event.preventDefault(); }}
          onInteractOutside={event => { if (isModelConfigSaving) event.preventDefault(); }}
        >
          <DialogHeader>
            <DialogTitle>AI model settings</DialogTitle>
            <DialogDescription>Choose the model used for summaries and AI chat.</DialogDescription>
          </DialogHeader>
          <ModelConfigForm layout="dialog" showCancel onCancel={() => onClose('modelSettings')} onCommitted={() => onClose('modelSettings')} />
        </DialogContent>
      </Dialog>

      <Dialog open={modals.deviceSettings} onOpenChange={open => closeOnChange('deviceSettings', open)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Audio device settings</DialogTitle>
            <DialogDescription>Choose the microphone and system audio source used for new recordings.</DialogDescription>
          </DialogHeader>
          <DeviceSelection selectedDevices={selectedDevices} onDeviceChange={setSelectedDevices} disabled={isRecording} />
          <DialogFooter>
            <Button onClick={() => {
              toast.success('Audio devices selected', {
                description: `Microphone: ${selectedDevices.micDevice || 'Default'}, System audio: ${selectedDevices.systemDevice || 'Default'}`,
              });
              onClose('deviceSettings');
            }}>Done</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={modals.languageSettings} onOpenChange={open => closeOnChange('languageSettings', open)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Language settings</DialogTitle>
            <DialogDescription>Choose the primary language used for transcription.</DialogDescription>
          </DialogHeader>
          <LanguageSelection selectedLanguage={selectedLanguage} onLanguageChange={setSelectedLanguage} disabled={isRecording} provider={transcriptModelConfig.provider} />
          <DialogFooter><Button onClick={() => onClose('languageSettings')}>Done</Button></DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={modals.modelSelector} onOpenChange={open => closeOnChange('modelSelector', open)}>
        <DialogContent className="max-h-[90vh] max-w-4xl overflow-y-auto">
          <DialogHeader>
            <DialogTitle>{messages.modelSelector ? 'Speech recognition setup required' : 'Transcription model settings'}</DialogTitle>
            <DialogDescription>{messages.modelSelector || 'Choose the engine and model used to transcribe meetings.'}</DialogDescription>
          </DialogHeader>
          <TranscriptSettings
            transcriptModelConfig={transcriptModelConfig}
            setTranscriptModelConfig={setTranscriptModelConfig}
            onModelSelect={() => onClose('modelSelector')}
          />
          <DialogFooter className="items-center sm:justify-between">
            <label className="flex items-center gap-3 text-sm text-ink">
              <Switch aria-label="Show transcription confidence indicators" checked={showConfidenceIndicator} onCheckedChange={toggleConfidenceIndicator} />
              Show confidence indicators
            </label>
            <Button variant="outline" onClick={() => onClose('modelSelector')}>{messages.modelSelector ? 'Cancel' : 'Done'}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={modals.errorAlert} onOpenChange={open => closeOnChange('errorAlert', open)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Recording stopped</DialogTitle>
            <DialogDescription>The recording could not continue.</DialogDescription>
          </DialogHeader>
          <Alert variant="destructive"><AlertDescription>{messages.errorAlert}</AlertDescription></Alert>
          <DialogFooter><Button variant="outline" onClick={() => onClose('errorAlert')}>Dismiss</Button></DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={modals.chunkDropWarning} onOpenChange={open => closeOnChange('chunkDropWarning', open)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Transcription performance warning</DialogTitle>
            <DialogDescription>Some audio could not be processed in real time.</DialogDescription>
          </DialogHeader>
          <Alert variant="warning"><AlertDescription>{messages.chunkDropWarning}</AlertDescription></Alert>
          <DialogFooter><Button variant="outline" onClick={() => onClose('chunkDropWarning')}>Dismiss</Button></DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
