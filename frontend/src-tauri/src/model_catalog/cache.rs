//! Process-global, session-lifetime cache for the OpenAI-shaped catalog.
//!
//! The ticket asks for each provider's model list to be fetched once and
//! cached for the session, so this is a plain in-memory map, not a TTL
//! cache: the three provider modules this replaces each carried a five
//! minute timer, but a timer has no place in a "once per session" cache. The
//! settings form's manual "Refresh" button is how a user forces a re-fetch —
//! it calls [`clear`] for the provider whose key changed, and the next
//! `list_models` call repopulates the entry.
//!
//! Entries are keyed on the provider *and* a fingerprint of the API key used
//! to fetch it, never the key itself. Keying on the key means a user who
//! pastes a new key never sees the previous key's models; not storing the
//! key means a process-global map is never the place a secret leaks from.
//! `None`, `Some("")` and `Some("   ")` all mean "no key" to every caller in
//! this module, so they fingerprint identically.

use super::{CatalogModel, CatalogProvider};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::RwLock;

/// Identity of one cache entry: a provider plus a fingerprint of the API
/// key that fetched it.
///
/// Deliberately does not derive `Debug`. A fingerprint is not a secret on
/// its own, but a struct that prints cleanly next to the cached models is
/// exactly the kind of thing that ends up in a `{:?}` log line by accident;
/// leaving `Debug` off keeps that from ever compiling.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct CacheKey {
    provider: CatalogProvider,
    key_fingerprint: u64,
}

/// The cache itself. `None` until the first write, so the static can be
/// built with a `const` initializer (`HashMap::new()` is not `const`).
static CACHE: RwLock<Option<HashMap<CacheKey, Vec<CatalogModel>>>> = RwLock::new(None);

/// Fingerprints an API key for use as a cache key, never for anything else.
///
/// Blank and absent keys are normalised to the same fingerprint, because
/// every caller in this module already treats them as "no key" — narrowing
/// here means `cached`/`store`/`clear` don't each have to repeat the
/// trim-and-check.
fn fingerprint(api_key: Option<&str>) -> u64 {
    let normalized = api_key.map(str::trim).filter(|k| !k.is_empty());
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    hasher.finish()
}

/// Looks up a provider's cached models for the given API key.
///
/// A poisoned lock (some other write panicked mid-update) degrades to a
/// miss rather than propagating an error or panicking itself — every caller
/// of this module already has a fallback path (fetch, then the hardcoded
/// list), so a cache that can't be read is no different from an empty one.
pub fn cached(provider: CatalogProvider, api_key: Option<&str>) -> Option<Vec<CatalogModel>> {
    let key = CacheKey {
        provider,
        key_fingerprint: fingerprint(api_key),
    };
    let guard = match CACHE.read() {
        Ok(guard) => guard,
        Err(_) => {
            log::warn!("model_catalog cache lock poisoned; treating as a cache miss");
            return None;
        }
    };
    guard.as_ref()?.get(&key).cloned()
}

/// Records a provider's fetched models under the given API key.
///
/// A write into a poisoned lock still recovers the guard (the poisoning
/// flags a panic elsewhere, not corrupt data here) so the cache keeps
/// working instead of staying broken for the rest of the session.
pub fn store(provider: CatalogProvider, api_key: Option<&str>, models: &[CatalogModel]) {
    let key = CacheKey {
        provider,
        key_fingerprint: fingerprint(api_key),
    };
    let mut guard = CACHE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .get_or_insert_with(HashMap::new)
        .insert(key, models.to_vec());
}

/// Evicts every cached entry for a provider, whatever key fetched it.
///
/// This is what the settings form's manual "Refresh" calls: it forces the
/// next `list_models` for that provider to hit the network again.
pub fn clear(provider: CatalogProvider) {
    let mut guard = CACHE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(map) = guard.as_mut() {
        map.retain(|entry_key, _| entry_key.provider != provider);
    }
}

/// Evicts every cached entry for every provider.
pub fn clear_all() {
    let mut guard = CACHE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // The cache above is one process-global static, and `cargo test` runs
    // tests on multiple threads by default. Serialising through this lock
    // is simpler than trying to give every test a provider/key combination
    // no other test could ever touch (there are only three providers).
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn store_then_read_round_trips() {
        let _guard = lock();
        let models = vec![CatalogModel::from_id("gpt-5")];
        store(CatalogProvider::OpenAI, Some("round-trip-key"), &models);
        assert_eq!(
            cached(CatalogProvider::OpenAI, Some("round-trip-key")),
            Some(models)
        );
    }

    #[test]
    fn different_key_same_provider_is_a_miss() {
        let _guard = lock();
        let models = vec![CatalogModel::from_id("llama-4")];
        store(CatalogProvider::Groq, Some("groq-key-a"), &models);
        assert_eq!(cached(CatalogProvider::Groq, Some("groq-key-b")), None);
    }

    #[test]
    fn same_key_different_provider_is_a_miss() {
        let _guard = lock();
        let models = vec![CatalogModel::from_id("shared-name-model")];
        store(
            CatalogProvider::OpenRouter,
            Some("shared-fingerprint-key"),
            &models,
        );
        assert_eq!(
            cached(CatalogProvider::OpenAI, Some("shared-fingerprint-key")),
            None
        );
    }

    #[test]
    fn blank_and_absent_keys_hit_the_same_entry() {
        let _guard = lock();
        let models = vec![CatalogModel::from_id("no-key-model")];
        store(CatalogProvider::OpenRouter, None, &models);
        assert_eq!(
            cached(CatalogProvider::OpenRouter, Some("")),
            Some(models.clone())
        );
        assert_eq!(
            cached(CatalogProvider::OpenRouter, Some("   ")),
            Some(models.clone())
        );
        assert_eq!(cached(CatalogProvider::OpenRouter, None), Some(models));
    }

    #[test]
    fn clear_evicts_one_provider_and_leaves_another_intact() {
        let _guard = lock();
        let openai_models = vec![CatalogModel::from_id("clear-test-openai-model")];
        let groq_models = vec![CatalogModel::from_id("clear-test-groq-model")];
        store(
            CatalogProvider::OpenAI,
            Some("clear-test-key"),
            &openai_models,
        );
        store(CatalogProvider::Groq, Some("clear-test-key"), &groq_models);

        clear(CatalogProvider::OpenAI);

        assert_eq!(
            cached(CatalogProvider::OpenAI, Some("clear-test-key")),
            None
        );
        assert_eq!(
            cached(CatalogProvider::Groq, Some("clear-test-key")),
            Some(groq_models)
        );
    }

    #[test]
    fn empty_model_list_is_cached_as_a_hit_not_a_miss() {
        let _guard = lock();
        // An empty list is a legitimate answer from a provider (an account
        // with no eligible models), and it is different from "we never
        // asked" — collapsing the two would make `cached` re-fetch forever
        // for that account.
        store(CatalogProvider::OpenAI, Some("empty-list-key"), &[]);
        assert_eq!(
            cached(CatalogProvider::OpenAI, Some("empty-list-key")),
            Some(vec![])
        );
    }

    #[test]
    fn clear_all_evicts_every_provider() {
        let _guard = lock();
        store(
            CatalogProvider::OpenAI,
            Some("clear-all-key"),
            &[CatalogModel::from_id("clear-all-openai-model")],
        );
        store(
            CatalogProvider::OpenRouter,
            Some("clear-all-key"),
            &[CatalogModel::from_id("clear-all-openrouter-model")],
        );

        clear_all();

        assert_eq!(cached(CatalogProvider::OpenAI, Some("clear-all-key")), None);
        assert_eq!(
            cached(CatalogProvider::OpenRouter, Some("clear-all-key")),
            None
        );
    }
}
