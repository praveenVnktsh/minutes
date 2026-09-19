'use client';

import type { Dispatch, SetStateAction } from 'react';
import { ModelConfigForm } from '@/components/settings/ModelConfigForm';
import type { ModelConfig } from '@/types/modelConfig';

export type { ModelConfig } from '@/types/modelConfig';

export interface ModelSettingsModalProps {
  modelConfig?: ModelConfig;
  setModelConfig?: Dispatch<SetStateAction<ModelConfig>>;
  /** @deprecated Persistence is owned by ModelConfigForm. This callback is intentionally not called. */
  onSave?: (config: ModelConfig) => void | Promise<void>;
  /** Called only after the committed configuration owner confirms persistence. */
  onCommitted?: (config: ModelConfig) => void;
  onCancel?: () => void;
  skipInitialFetch?: boolean;
  layout?: 'inline' | 'dialog';
  showCancel?: boolean;
}

/**
 * Compatibility shell for existing quick editors. Draft and persistence state
 * deliberately live in ModelConfigForm rather than legacy parent callbacks.
 */
export function ModelSettingsModal({
  onCommitted,
  onCancel,
  layout = 'inline',
  showCancel = layout === 'dialog',
}: ModelSettingsModalProps) {
  return (
    <ModelConfigForm
      layout={layout}
      onCommitted={onCommitted}
      onCancel={onCancel}
      showCancel={showCancel}
    />
  );
}
