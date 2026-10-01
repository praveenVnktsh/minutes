import { useCallback } from 'react';
import Analytics from '@/lib/analytics';
import { useConfig } from '@/contexts/ConfigContext';
import type { ModelConfig } from '@/types/modelConfig';

export function useModelConfiguration() {
  const {
    modelConfig,
    setModelConfig,
    commitModelConfig,
    isModelConfigLoading,
  } = useConfig();

  // Save model configuration
  const handleSaveModelConfig = useCallback(async (updatedConfig?: ModelConfig) => {
    const configToSave = updatedConfig ?? modelConfig;
    const committed = await commitModelConfig(configToSave);
    try {
      if (committed.provider !== modelConfig.provider || committed.model !== modelConfig.model) {
        await Analytics.trackModelChanged(
          modelConfig.provider,
          modelConfig.model,
          committed.provider,
          committed.model,
        );
      }
      await Analytics.trackSettingsChanged('model_config', `${committed.provider}_${committed.model}`);
    } catch (error) {
      console.warn('Model configuration saved, but analytics tracking failed:', error);
    }
    return committed;
  }, [commitModelConfig, modelConfig]);

  return {
    modelConfig,
    setModelConfig,
    handleSaveModelConfig,
    isLoading: isModelConfigLoading,
  };
}
