//! cooldown — 自 src/tools/web.rs 拆分。

pub(crate) use super::*;

pub(crate) fn rotate_from<T>(mut items: Vec<T>, start: usize) -> Vec<T> {
    items.rotate_left(start);
    items
}

pub(crate) fn provider_cooldown_id(provider: &str) -> String {
    format!("provider:{provider}")
}

pub(crate) fn key_cooldown_id(provider: &str, index: usize) -> String {
    format!("key:{provider}:{index}")
}

pub(crate) fn has_non_empty_key(keys: &[String]) -> bool {
    keys.iter().any(|key| !key.trim().is_empty())
}

pub(crate) fn configured_primary_providers(config: &WebPluginConfig) -> Vec<SearchProvider> {
    let mut providers = Vec::new();
    if has_non_empty_key(&config.tavily_api_keys) {
        providers.push(SearchProvider::Tavily);
    }
    if has_non_empty_key(&config.firecrawl_api_keys) {
        providers.push(SearchProvider::Firecrawl);
    }
    if has_non_empty_key(&config.anysearch_api_keys) {
        providers.push(SearchProvider::AnySearch);
    }
    if has_non_empty_key(&config.exa_api_keys) {
        providers.push(SearchProvider::Exa);
    }
    if !config.searxng_base_url.trim().is_empty() {
        providers.push(SearchProvider::SearXng);
    }
    providers
}

pub(crate) fn ordered_providers(providers: &[SearchProvider]) -> Vec<SearchProvider> {
    SEARCH_SCHEDULER
        .lock()
        .map(|mut scheduler| scheduler.ordered_providers(providers))
        .unwrap_or_else(|_| providers.to_vec())
}

pub(crate) fn ordered_key_positions(provider: &'static str, key_count: usize) -> Vec<usize> {
    SEARCH_SCHEDULER
        .lock()
        .map(|mut scheduler| scheduler.ordered_key_positions(provider, key_count))
        .unwrap_or_else(|_| (0..key_count).collect())
}

pub(crate) fn mark_provider_success(provider: &str) {
    if let Ok(mut scheduler) = SEARCH_SCHEDULER.lock() {
        scheduler.mark_success(&provider_cooldown_id(provider));
    }
}

pub(crate) fn mark_key_success(provider: &'static str, index: usize) {
    if let Ok(mut scheduler) = SEARCH_SCHEDULER.lock() {
        scheduler.mark_success(&key_cooldown_id(provider, index));
    }
}

pub(crate) fn mark_provider_failure(provider: &str, error: &str) {
    let Some(duration) = cooldown_for_error(error) else {
        return;
    };
    if let Ok(mut scheduler) = SEARCH_SCHEDULER.lock() {
        scheduler.mark_failure(provider_cooldown_id(provider), duration);
    }
}

pub(crate) fn mark_key_failure(provider: &'static str, index: usize, error: &str) {
    let Some(duration) = cooldown_for_error(error) else {
        return;
    };
    if let Ok(mut scheduler) = SEARCH_SCHEDULER.lock() {
        scheduler.mark_failure(key_cooldown_id(provider, index), duration);
    }
}

pub(crate) fn cooldown_for_status(status: u16) -> Option<Duration> {
    match status {
        401 | 403 | 429 => Some(Duration::from_secs(600)),
        408 | 500..=599 => Some(Duration::from_secs(120)),
        _ => None,
    }
}

pub(crate) fn cooldown_for_error(error: &str) -> Option<Duration> {
    let lower = error.to_ascii_lowercase();
    if lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("ratelimit")
        || lower.contains("quota")
    {
        return Some(Duration::from_secs(600));
    }
    if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("invalid api key")
    {
        return Some(Duration::from_secs(600));
    }
    if lower.contains("408")
        || lower.contains("500")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("504")
        || lower.contains("request failed")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection")
    {
        return Some(Duration::from_secs(120));
    }
    if lower.contains("captcha") || lower.contains("challenge") {
        return Some(Duration::from_secs(300));
    }
    None
}
