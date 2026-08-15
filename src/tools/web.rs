use super::{html_conversion, http_response, ToolRegistry, ToolSpec};
use crate::config::WebPluginConfig;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use urlencoding::decode as url_decode;

mod cooldown;
pub(crate) use cooldown::*;
mod providers;
pub(crate) use providers::*;
mod crawlers;
pub(crate) use crawlers::*;
mod parse;
pub(crate) use parse::*;
#[cfg(test)]
mod tests;
const MAX_RESPONSE_SIZE: usize = 5 * 1024 * 1024;
const DEFAULT_FETCH_MAX_CHARS: usize = 40_000;
const MAX_FETCH_CHARS: usize = 200_000;

const CRAWLER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
const CRAWLER_TIMEOUT: Duration = Duration::from_secs(15);

static DDG_BLOCKED_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
static SOGOU_BLOCKED_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
static SEARCH_SCHEDULER: LazyLock<Mutex<SearchScheduler>> =
    LazyLock::new(|| Mutex::new(SearchScheduler::default()));

pub(crate) struct CrawlerResult {
    title: String,
    url: String,
    snippet: String,
    source: String,
}

#[derive(Clone, Copy)]
pub(crate) enum SearchProvider {
    Tavily,
    Firecrawl,
    AnySearch,
    Exa,
    SearXng,
    DuckDuckGo,
}

impl SearchProvider {
    fn id(self) -> &'static str {
        match self {
            Self::Tavily => "tavily",
            Self::Firecrawl => "firecrawl",
            Self::AnySearch => "anysearch",
            Self::Exa => "exa",
            Self::SearXng => "searxng",
            Self::DuckDuckGo => "duckduckgo",
        }
    }
}

#[derive(Default)]
struct SearchScheduler {
    provider_cursor: usize,
    key_cursors: HashMap<&'static str, usize>,
    cooldowns: HashMap<String, Instant>,
}

impl SearchScheduler {
    fn ordered_providers(&mut self, providers: &[SearchProvider]) -> Vec<SearchProvider> {
        let available = providers
            .iter()
            .copied()
            .filter(|provider| self.is_ready(&provider_cooldown_id(provider.id())))
            .collect::<Vec<_>>();
        if available.is_empty() {
            return Vec::new();
        }
        let start = self.provider_cursor % available.len();
        self.provider_cursor = self.provider_cursor.wrapping_add(1);
        rotate_from(available, start)
    }

    fn ordered_key_positions(&mut self, provider: &'static str, key_count: usize) -> Vec<usize> {
        let available = (0..key_count)
            .filter(|&index| self.is_ready(&key_cooldown_id(provider, index)))
            .collect::<Vec<_>>();
        if available.is_empty() {
            return Vec::new();
        }
        let cursor = self.key_cursors.entry(provider).or_insert(0);
        let start = *cursor % available.len();
        *cursor = cursor.wrapping_add(1);
        rotate_from(available, start)
    }

    fn is_ready(&mut self, id: &str) -> bool {
        match self.cooldowns.get(id).copied() {
            Some(until) if until > Instant::now() => false,
            Some(_) => {
                self.cooldowns.remove(id);
                true
            }
            None => true,
        }
    }

    fn mark_success(&mut self, id: &str) {
        self.cooldowns.remove(id);
    }

    fn mark_failure(&mut self, id: String, duration: Duration) {
        self.cooldowns.insert(id, Instant::now() + duration);
    }
}

pub fn register(registry: &mut ToolRegistry, config: WebPluginConfig) {
    register_search_tool(registry, "web_search", config.clone());
}

pub fn register_fetch(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new(
        "web_fetch",
        "Fetch a URL and return markdown, text, or html. Prefer this for opening a known URL. Does not search the web.",
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "Fully-qualified http or https URL." },
                "format": { "type": "string", "enum": ["markdown", "text", "html"], "description": "Output format. Defaults to markdown." },
                "timeout": { "type": "integer", "description": "Timeout seconds, max 120." },
                "max_chars": { "type": "integer", "description": "Maximum characters to return. Defaults to 40000, max 200000." }
            },
            "required": ["url"],
            "additionalProperties": false
        }),
        |args| async move { web_fetch(args).await },
    ));
}

fn register_search_tool(registry: &mut ToolRegistry, name: &'static str, config: WebPluginConfig) {
    registry.register(ToolSpec::new(
        name,
        "Search the web. Prefer configured Tavily, Firecrawl, AnySearch, or Exa API keys; fallback to SearXNG, then Exa's keyless free quota, then built-in DuckDuckGo HTML search (with Yahoo/360/Sogou fallback) when providers fail.",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query." },
                "max_results": { "type": "integer", "description": "Maximum results; defaults to plugins.web.max_results." },
                "provider": { "type": "string", "enum": ["auto", "tavily", "firecrawl", "anysearch", "exa", "searxng", "script"], "description": "Search provider." }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        move |args| {
            let config = config.clone();
            async move { web_search(args, config).await }
        },
    ));
}

async fn web_search(args: Value, config: WebPluginConfig) -> Result<String> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if query.is_empty() {
        bail!("query is required");
    }
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(config.max_results as u64)
        .clamp(1, 10) as usize;
    let provider = args
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let client = reqwest::Client::builder()
        .timeout(CRAWLER_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;
    let order = search_provider_order(provider, &config)?;
    let mut errors = Vec::new();
    for item in order {
        let provider_id = item.id();
        let result = search_with_provider(&client, query, max_results, &config, item).await;
        match result {
            Ok(output) => {
                mark_provider_success(provider_id);
                return Ok(output);
            }
            Err(err) => {
                let message = err.to_string();
                mark_provider_failure(provider_id, &message);
                errors.push(format!("{provider_id}: {message}"));
            }
        }
    }
    bail!(
        "no web search provider succeeded:\n- {}",
        errors.join("\n- ")
    )
}

fn search_provider_order(provider: &str, config: &WebPluginConfig) -> Result<Vec<SearchProvider>> {
    if provider == "auto" {
        let mut providers = ordered_providers(&configured_primary_providers(config));
        // 未配置 key 时 Exa 走官方 MCP 免费公共额度：排在已配置服务之后、爬虫之前；
        // 报错/429 会通过 cooldown 自动让位给爬虫
        if !has_non_empty_key(&config.exa_api_keys)
            && SEARCH_SCHEDULER
                .lock()
                .map(|mut scheduler| scheduler.is_ready(&provider_cooldown_id("exa")))
                .unwrap_or(true)
        {
            providers.push(SearchProvider::Exa);
        }
        if SEARCH_SCHEDULER
            .lock()
            .map(|mut scheduler| scheduler.is_ready(&provider_cooldown_id("duckduckgo")))
            .unwrap_or(true)
        {
            providers.push(SearchProvider::DuckDuckGo);
        }
        if providers.is_empty() {
            return Ok(vec![SearchProvider::DuckDuckGo]);
        }
        return Ok(providers);
    }
    match provider {
        "tavily" => Ok(vec![SearchProvider::Tavily]),
        "firecrawl" => Ok(vec![SearchProvider::Firecrawl]),
        "anysearch" => Ok(vec![SearchProvider::AnySearch]),
        "exa" => Ok(vec![SearchProvider::Exa]),
        "searxng" => Ok(vec![SearchProvider::SearXng]),
        "duckduckgo" | "script" => Ok(vec![SearchProvider::DuckDuckGo]),
        _ => bail!("{provider}: unknown provider"),
    }
}

async fn search_fallback_html(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    let yahoo_results = search_yahoo_html(client, query, max_results).await;
    if yahoo_results.len() >= max_results.min(5) {
        return yahoo_results;
    }

    let mut combined: Vec<CrawlerResult> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in yahoo_results {
        let key = dedupe_key(&r.url);
        if seen.insert(key) {
            combined.push(r);
        }
    }

    let so_results = search_so_html(client, query, max_results).await;
    for r in so_results {
        if combined.len() >= max_results {
            break;
        }
        let key = dedupe_key(&r.url);
        if seen.insert(key) {
            combined.push(r);
        }
    }

    if combined.len() < max_results {
        let sogou_results = search_sogou_html(client, query, max_results).await;
        for r in sogou_results {
            if combined.len() >= max_results {
                break;
            }
            let key = dedupe_key(&r.url);
            if seen.insert(key) {
                combined.push(r);
            }
        }
    }

    combined
}

async fn web_fetch(args: Value) -> Result<String> {
    let url = args
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        bail!("URL must start with http:// or https://");
    }
    let format = args
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("markdown");
    let timeout = args
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .min(120);
    let max_chars = args
        .get("max_chars")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1, MAX_FETCH_CHARS as u64) as usize)
        .unwrap_or(DEFAULT_FETCH_MAX_CHARS);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout))
        .build()?;
    let accept = match format {
        "text" => "text/plain;q=1.0, text/markdown;q=0.9, text/html;q=0.8, */*;q=0.1",
        "html" => "text/html;q=1.0, application/xhtml+xml;q=0.9, text/plain;q=0.8, */*;q=0.1",
        _ => "text/markdown;q=1.0, text/x-markdown;q=0.9, text/plain;q=0.8, text/html;q=0.7, */*;q=0.1",
    };
    let response = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36")
        .header("Accept", accept)
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()
        .await?
        .error_for_status()?;
    if response.content_length().unwrap_or(0) > MAX_RESPONSE_SIZE as u64 {
        bail!("response too large (exceeds 5MB limit)");
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let content = http_response::read_text(response, MAX_RESPONSE_SIZE).await?;
    let output = if content_type.contains("text/html") {
        match format {
            "html" => content,
            "text" => html_conversion::to_text_async(content, 120).await?,
            _ => html_conversion::to_markdown(content).await?,
        }
    } else {
        content
    };
    Ok(clip_fetch_output(&output, max_chars))
}

fn clip_fetch_output(value: &str, max_chars: usize) -> String {
    let total = value.chars().count();
    if total <= max_chars {
        return value.to_string();
    }
    let clipped = value.chars().take(max_chars).collect::<String>();
    format!("{clipped}\n\n[content truncated from {total} chars to {max_chars} chars]")
}
