use crate::database::models::{Setting, TranscriptSetting};
use crate::summary::CustomOpenAIConfig;
use sqlx::{SqlitePool, Transaction};

#[derive(serde::Deserialize, Debug)]
pub struct SaveModelConfigRequest {
    pub provider: String,
    pub model: String,
    #[serde(rename = "whisperModel")]
    pub whisper_model: String,
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
    #[serde(rename = "ollamaEndpoint")]
    pub ollama_endpoint: Option<String>,
}

#[derive(serde::Deserialize, Debug)]
pub struct SaveTranscriptConfigRequest {
    pub provider: String,
    pub model: String,
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
}

pub struct SettingsRepository;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeyUpdate {
    Preserve,
    Set(String),
    Clear,
}

// Transcript providers: localWhisper, deepgram, elevenLabs, groq, openai
// Summary providers: openai, claude, ollama, groq, added openrouter
// NOTE: Handle data exclusion in the higher layer as this is database abstraction layer(using SELECT *)

impl SettingsRepository {
    fn api_key_column(provider: &str) -> std::result::Result<Option<&'static str>, sqlx::Error> {
        match provider {
            "openai" => Ok(Some("openaiApiKey")),
            "claude" => Ok(Some("anthropicApiKey")),
            "ollama" => Ok(Some("ollamaApiKey")),
            "groq" => Ok(Some("groqApiKey")),
            "openrouter" => Ok(Some("openRouterApiKey")),
            "builtin-ai" | "custom-openai" => Ok(None),
            _ => Err(sqlx::Error::Protocol(format!(
                "Invalid provider: {}",
                provider
            ))),
        }
    }

    pub async fn get_model_config(
        pool: &SqlitePool,
    ) -> std::result::Result<Option<Setting>, sqlx::Error> {
        let setting = sqlx::query_as::<_, Setting>("SELECT * FROM settings LIMIT 1")
            .fetch_optional(pool)
            .await?;
        Ok(setting)
    }

    pub async fn save_model_config(
        pool: &SqlitePool,
        provider: &str,
        model: &str,
        whisper_model: &str,
        ollama_endpoint: Option<&str>,
    ) -> std::result::Result<(), sqlx::Error> {
        // Using id '1' for backward compatibility
        sqlx::query(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, ollamaEndpoint)
            VALUES ('1', $1, $2, $3, $4)
            ON CONFLICT(id) DO UPDATE SET
                provider = excluded.provider,
                model = excluded.model,
                whisperModel = excluded.whisperModel,
                ollamaEndpoint = excluded.ollamaEndpoint
            "#,
        )
        .bind(provider)
        .bind(model)
        .bind(whisper_model)
        .bind(ollama_endpoint)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn save_model_config_transaction(
        pool: &SqlitePool,
        provider: &str,
        model: &str,
        whisper_model: &str,
        ollama_endpoint: Option<&str>,
        api_key_update: ApiKeyUpdate,
        custom_openai_config: Option<&CustomOpenAIConfig>,
    ) -> std::result::Result<Setting, sqlx::Error> {
        let api_key_column = Self::api_key_column(provider)?;
        if provider == "custom-openai" && matches!(&api_key_update, ApiKeyUpdate::Set(_)) {
            return Err(sqlx::Error::Protocol(
                "custom-openai credentials belong in customOpenAIConfig".into(),
            ));
        }

        let custom_config_json = custom_openai_config
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let mut transaction = pool.begin().await?;

        Self::upsert_model_config(
            &mut transaction,
            provider,
            model,
            whisper_model,
            ollama_endpoint,
        )
        .await?;

        if let Some(custom_config_json) = custom_config_json {
            sqlx::query("UPDATE settings SET customOpenAIConfig = $1 WHERE id = '1'")
                .bind(custom_config_json)
                .execute(&mut *transaction)
                .await?;
        }

        if let Some(column) = api_key_column {
            match api_key_update {
                ApiKeyUpdate::Preserve => {}
                ApiKeyUpdate::Set(api_key) => {
                    let query = format!("UPDATE settings SET {} = $1 WHERE id = '1'", column);
                    sqlx::query(&query)
                        .bind(api_key)
                        .execute(&mut *transaction)
                        .await?;
                }
                ApiKeyUpdate::Clear => {
                    let query = format!("UPDATE settings SET {} = NULL WHERE id = '1'", column);
                    sqlx::query(&query).execute(&mut *transaction).await?;
                }
            }
        }

        let committed = sqlx::query_as::<_, Setting>("SELECT * FROM settings WHERE id = '1'")
            .fetch_one(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(committed)
    }

    async fn upsert_model_config(
        transaction: &mut Transaction<'_, sqlx::Sqlite>,
        provider: &str,
        model: &str,
        whisper_model: &str,
        ollama_endpoint: Option<&str>,
    ) -> std::result::Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, ollamaEndpoint)
            VALUES ('1', $1, $2, $3, $4)
            ON CONFLICT(id) DO UPDATE SET
                provider = excluded.provider,
                model = excluded.model,
                whisperModel = excluded.whisperModel,
                ollamaEndpoint = excluded.ollamaEndpoint
            "#,
        )
        .bind(provider)
        .bind(model)
        .bind(whisper_model)
        .bind(ollama_endpoint)
        .execute(&mut **transaction)
        .await?;
        Ok(())
    }

    pub async fn save_api_key(
        pool: &SqlitePool,
        provider: &str,
        api_key: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        // Custom OpenAI uses JSON config (customOpenAIConfig) instead of a separate API key column
        if provider == "custom-openai" {
            return Err(sqlx::Error::Protocol(
                "custom-openai provider should use save_custom_openai_config() instead of save_api_key()".into(),
            ));
        }

        let Some(api_key_column) = Self::api_key_column(provider)? else {
            return Ok(());
        };

        let query = format!(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, "{}")
            VALUES ('1', 'openai', 'gpt-4o-2024-11-20', 'large-v3', $1)
            ON CONFLICT(id) DO UPDATE SET
                "{}" = $1
            "#,
            api_key_column, api_key_column
        );
        sqlx::query(&query).bind(api_key).execute(pool).await?;

        Ok(())
    }

    pub async fn get_api_key(
        pool: &SqlitePool,
        provider: &str,
    ) -> std::result::Result<Option<String>, sqlx::Error> {
        // Custom OpenAI uses JSON config - extract API key from there
        if provider == "custom-openai" {
            let config = Self::get_custom_openai_config(pool).await?;
            return Ok(config.and_then(|c| c.api_key));
        }

        let api_key_column = match provider {
            "openai" => "openaiApiKey",
            "ollama" => "ollamaApiKey",
            "groq" => "groqApiKey",
            "claude" => "anthropicApiKey",
            "openrouter" => "openRouterApiKey",
            "builtin-ai" => return Ok(None), // No API key needed
            _ => {
                return Err(sqlx::Error::Protocol(format!(
                    "Invalid provider: {}",
                    provider
                )))
            }
        };

        let query = format!(
            "SELECT {} FROM settings WHERE id = '1' LIMIT 1",
            api_key_column
        );
        let api_key = sqlx::query_scalar(&query).fetch_optional(pool).await?;
        Ok(api_key)
    }

    pub async fn get_transcript_config(
        pool: &SqlitePool,
    ) -> std::result::Result<Option<TranscriptSetting>, sqlx::Error> {
        let setting =
            sqlx::query_as::<_, TranscriptSetting>("SELECT * FROM transcript_settings LIMIT 1")
                .fetch_optional(pool)
                .await?;
        Ok(setting)
    }

    pub async fn save_transcript_config(
        pool: &SqlitePool,
        provider: &str,
        model: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO transcript_settings (id, provider, model)
            VALUES ('1', $1, $2)
            ON CONFLICT(id) DO UPDATE SET
                provider = excluded.provider,
                model = excluded.model
            "#,
        )
        .bind(provider)
        .bind(model)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn save_transcript_api_key(
        pool: &SqlitePool,
        provider: &str,
        api_key: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        let api_key_column = match provider {
            "localWhisper" => "whisperApiKey",
            "parakeet" => return Ok(()), // Parakeet doesn't need an API key, return early
            "deepgram" => "deepgramApiKey",
            "elevenLabs" => "elevenLabsApiKey",
            "groq" => "groqApiKey",
            "openai" => "openaiApiKey",
            _ => {
                return Err(sqlx::Error::Protocol(format!(
                    "Invalid provider: {}",
                    provider
                )))
            }
        };

        let query = format!(
            r#"
            INSERT INTO transcript_settings (id, provider, model, "{}")
            VALUES ('1', 'parakeet', '{}', $1)
            ON CONFLICT(id) DO UPDATE SET
                "{}" = $1
            "#,
            api_key_column,
            crate::config::DEFAULT_PARAKEET_MODEL,
            api_key_column
        );
        sqlx::query(&query).bind(api_key).execute(pool).await?;

        Ok(())
    }

    pub async fn get_transcript_api_key(
        pool: &SqlitePool,
        provider: &str,
    ) -> std::result::Result<Option<String>, sqlx::Error> {
        let api_key_column = match provider {
            "localWhisper" => "whisperApiKey",
            "parakeet" => return Ok(None), // Parakeet doesn't need an API key
            "deepgram" => "deepgramApiKey",
            "elevenLabs" => "elevenLabsApiKey",
            "groq" => "groqApiKey",
            "openai" => "openaiApiKey",
            _ => {
                return Err(sqlx::Error::Protocol(format!(
                    "Invalid provider: {}",
                    provider
                )))
            }
        };

        let query = format!(
            "SELECT {} FROM transcript_settings WHERE id = '1' LIMIT 1",
            api_key_column
        );
        let api_key = sqlx::query_scalar(&query).fetch_optional(pool).await?;
        Ok(api_key)
    }

    pub async fn delete_api_key(
        pool: &SqlitePool,
        provider: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        // Custom OpenAI uses JSON config - clear the entire config
        if provider == "custom-openai" {
            sqlx::query("UPDATE settings SET customOpenAIConfig = NULL WHERE id = '1'")
                .execute(pool)
                .await?;
            return Ok(());
        }

        let api_key_column = match provider {
            "openai" => "openaiApiKey",
            "ollama" => "ollamaApiKey",
            "groq" => "groqApiKey",
            "claude" => "anthropicApiKey",
            "openrouter" => "openRouterApiKey",
            "builtin-ai" => return Ok(()), // No API key needed
            _ => {
                return Err(sqlx::Error::Protocol(format!(
                    "Invalid provider: {}",
                    provider
                )))
            }
        };

        let query = format!(
            "UPDATE settings SET {} = NULL WHERE id = '1'",
            api_key_column
        );
        sqlx::query(&query).execute(pool).await?;

        Ok(())
    }

    // ===== CUSTOM OPENAI CONFIG METHODS =====

    /// Gets the custom OpenAI configuration from JSON
    ///
    /// # Returns
    /// * `Ok(Some(CustomOpenAIConfig))` - Config exists and is valid JSON
    /// * `Ok(None)` - No config stored
    /// * `Err(sqlx::Error)` - Database error
    pub async fn get_custom_openai_config(
        pool: &SqlitePool,
    ) -> std::result::Result<Option<CustomOpenAIConfig>, sqlx::Error> {
        use sqlx::Row;

        let row = sqlx::query(
            r#"
            SELECT customOpenAIConfig
            FROM settings
            WHERE id = '1'
            LIMIT 1
            "#,
        )
        .fetch_optional(pool)
        .await?;

        match row {
            Some(record) => {
                let config_json: Option<String> = record.get("customOpenAIConfig");

                if let Some(json) = config_json {
                    // Parse JSON into CustomOpenAIConfig
                    let config: CustomOpenAIConfig = serde_json::from_str(&json).map_err(|e| {
                        sqlx::Error::Protocol(format!("Invalid JSON in customOpenAIConfig: {}", e))
                    })?;

                    Ok(Some(config))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    /// Saves the custom OpenAI configuration as JSON
    ///
    /// # Arguments
    /// * `pool` - Database connection pool
    /// * `config` - CustomOpenAIConfig to save (includes endpoint, apiKey, model, maxTokens, temperature, topP)
    ///
    /// # Returns
    /// * `Ok(())` - Config saved successfully
    /// * `Err(sqlx::Error)` - Database or JSON serialization error
    pub async fn save_custom_openai_config(
        pool: &SqlitePool,
        config: &CustomOpenAIConfig,
    ) -> std::result::Result<(), sqlx::Error> {
        // Serialize config to JSON
        let config_json = serde_json::to_string(config).map_err(|e| {
            sqlx::Error::Protocol(format!("Failed to serialize config to JSON: {}", e))
        })?;

        // Upsert into settings table
        sqlx::query(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, customOpenAIConfig)
            VALUES ('1', 'custom-openai', $1, 'large-v3', $2)
            ON CONFLICT(id) DO UPDATE SET
                customOpenAIConfig = excluded.customOpenAIConfig
            "#,
        )
        .bind(&config.model)
        .bind(config_json)
        .execute(pool)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            r#"
            CREATE TABLE settings (
                id TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                whisperModel TEXT NOT NULL,
                groqApiKey TEXT,
                openaiApiKey TEXT,
                anthropicApiKey TEXT,
                ollamaApiKey TEXT,
                openRouterApiKey TEXT,
                ollamaEndpoint TEXT,
                customOpenAIConfig TEXT
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn transaction_preserves_and_explicitly_clears_provider_key() {
        let pool = test_pool().await;
        SettingsRepository::save_model_config_transaction(
            &pool,
            "openai",
            "gpt-4o",
            "large-v3",
            None,
            ApiKeyUpdate::Set("secret".to_string()),
            None,
        )
        .await
        .unwrap();

        let preserved = SettingsRepository::save_model_config_transaction(
            &pool,
            "openai",
            "gpt-4.1",
            "large-v3",
            None,
            ApiKeyUpdate::Preserve,
            None,
        )
        .await
        .unwrap();
        assert_eq!(preserved.openai_api_key.as_deref(), Some("secret"));

        let cleared = SettingsRepository::save_model_config_transaction(
            &pool,
            "openai",
            "gpt-4.1",
            "large-v3",
            None,
            ApiKeyUpdate::Clear,
            None,
        )
        .await
        .unwrap();
        assert_eq!(cleared.openai_api_key, None);
    }

    #[tokio::test]
    async fn second_write_failure_rolls_back_base_configuration() {
        let pool = test_pool().await;
        SettingsRepository::save_model_config_transaction(
            &pool,
            "openai",
            "old-model",
            "large-v3",
            None,
            ApiKeyUpdate::Set("old-key".to_string()),
            None,
        )
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER reject_key BEFORE UPDATE OF openaiApiKey ON settings \
             WHEN NEW.openaiApiKey = 'rejected' BEGIN SELECT RAISE(FAIL, 'rejected key'); END",
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = SettingsRepository::save_model_config_transaction(
            &pool,
            "openai",
            "new-model",
            "small",
            None,
            ApiKeyUpdate::Set("rejected".to_string()),
            None,
        )
        .await;
        assert!(result.is_err());

        let stored = SettingsRepository::get_model_config(&pool)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.model, "old-model");
        assert_eq!(stored.whisper_model, "large-v3");
        assert_eq!(stored.openai_api_key.as_deref(), Some("old-key"));
    }

    #[tokio::test]
    async fn custom_write_failure_rolls_back_base_configuration() {
        let pool = test_pool().await;
        SettingsRepository::save_model_config_transaction(
            &pool,
            "ollama",
            "llama3",
            "large-v3",
            None,
            ApiKeyUpdate::Preserve,
            None,
        )
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER reject_custom BEFORE UPDATE OF customOpenAIConfig ON settings \
             BEGIN SELECT RAISE(FAIL, 'rejected custom config'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        let custom = CustomOpenAIConfig {
            endpoint: "https://example.test/v1".to_string(),
            api_key: Some("secret".to_string()),
            model: "private-model".to_string(),
            max_tokens: Some(1024),
            temperature: Some(0.5),
            top_p: Some(0.9),
        };

        let result = SettingsRepository::save_model_config_transaction(
            &pool,
            "custom-openai",
            "private-model",
            "small",
            None,
            ApiKeyUpdate::Preserve,
            Some(&custom),
        )
        .await;
        assert!(result.is_err());

        let stored = SettingsRepository::get_model_config(&pool)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.provider, "ollama");
        assert_eq!(stored.model, "llama3");
        assert_eq!(stored.custom_openai_config, None);
    }
}
