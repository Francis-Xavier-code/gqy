//! images — 自 src/tools/web_images.rs 拆分。

use super::*;

use super::{vision, ToolProgress, ToolRegistry, ToolSpec};
use crate::config::{AppConfig, ProviderConfig, VisionPluginConfig};
use crate::i18n::{agent_text, text as t};
use crate::llm::{ChatMessage, OpenAiCompatibleClient};
use crate::paths::GQYPaths;
use anyhow::{bail, Context, Result};
use futures_util::{future::join_all, StreamExt};
use image::{DynamicImage, GenericImageView, ImageBuffer, ImageFormat, Rgb, RgbImage};
use reqwest::{Client, Url};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::net::IpAddr;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

pub(crate) const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
pub(crate) const MAX_IMAGE_PIXELS: u64 = 16_000_000;
pub(crate) const IMAGE_DECODER_MAX_ALLOC: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_DOWNLOAD_BYTES: usize = 50 * 1024 * 1024;
pub(crate) const MAX_SEARCH_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) static PROVIDER_COOLDOWNS: LazyLock<Mutex<HashMap<&'static str, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub(crate) static IMAGE_DECODE_PERMITS: LazyLock<std::sync::Arc<Semaphore>> =
    LazyLock::new(|| std::sync::Arc::new(Semaphore::new(4)));
pub(crate) static CACHE_PUBLISH_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

#[derive(Debug, Clone)]
pub(crate) struct ImageCandidate {
    pub(crate) title: String,
    pub(crate) page_url: String,
    pub(crate) image_url: String,
    pub(crate) thumbnail_url: String,
    pub(crate) source: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) search_description: String,
    pub(crate) provider_rank: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ImageSearchProvider {
    SearXng,
    DuckDuckGo,
    BingCn,
    Baidu,
    So360,
}

impl ImageSearchProvider {
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::SearXng => "searxng",
            Self::DuckDuckGo => "duckduckgo",
            Self::BingCn => "bing_cn",
            Self::Baidu => "baidu",
            Self::So360 => "so360",
        }
    }
}

pub(crate) struct ImageSearchResult {
    candidates: Vec<ImageCandidate>,
    diagnostics: Vec<Value>,
}

pub(crate) struct StoredImage {
    pub(crate) candidate: ImageCandidate,
    pub(crate) local_path: PathBuf,
    pub(crate) mime_type: String,
    pub(crate) size_bytes: usize,
    pub(crate) sha256: String,
    pub(crate) used_thumbnail: bool,
    pub(crate) vision: VisionScreening,
}

pub(crate) struct CallTempDir {
    inner: Option<tempfile::TempDir>,
}

impl CallTempDir {
    pub(crate) fn new(cache_dir: &Path) -> Result<Self> {
        Ok(Self {
            inner: Some(
                tempfile::Builder::new()
                    .prefix(".webimg-call-")
                    .tempdir_in(cache_dir)
                    .with_context(|| {
                        format!("failed to create image temp dir in {}", cache_dir.display())
                    })?,
            ),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        self.inner.as_ref().expect("temp dir is available").path()
    }
}

impl Drop for CallTempDir {
    fn drop(&mut self) {
        if let Some(dir) = self.inner.take() {
            let path = dir.path().to_path_buf();
            if let Err(error) = dir.close() {
                tracing::warn!(
                    error = %error,
                    path = %path.display(),
                    "failed to clean web image call temp directory"
                );
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct VisionScreening {
    status: String,
    pub(crate) accepted: bool,
    description: String,
    reason: String,
    provider_id: String,
    model: String,
    error: String,
    relevance: u8,
    quality: u8,
    pub(crate) safe: bool,
}

impl VisionScreening {
    pub(crate) fn not_requested() -> Self {
        Self {
            status: "not_requested".to_string(),
            accepted: true,
            description: String::new(),
            reason: String::new(),
            provider_id: String::new(),
            model: String::new(),
            error: String::new(),
            relevance: 100,
            quality: 50,
            safe: true,
        }
    }

    pub(crate) fn failed(error: impl Into<String>, provider: Option<&ProviderConfig>) -> Self {
        Self {
            status: "failed".to_string(),
            accepted: false,
            description: String::new(),
            reason: String::new(),
            provider_id: provider.map(|item| item.id.clone()).unwrap_or_default(),
            model: provider
                .map(|item| item.default_model.clone())
                .unwrap_or_default(),
            error: error.into(),
            relevance: 50,
            quality: 50,
            safe: false,
        }
    }
}

pub fn register(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: GQYPaths,
    allow_download: bool,
) {
    registry.register(ToolSpec::new_with_progress(
        "search_web_images",
        agent_text(
            "Search web images with parallel multi-source retrieval, ranking, deduplication, and optional vision review. Sources adapt to global or mainland connectivity and can include SearXNG, DuckDuckGo, Bing CN, Baidu, and 360.",
            "并行使用多个来源搜索网络图片，统一排序、去重并可进行视觉审核。搜索来源会适配全球或中国大陆网络，可包括 SearXNG、DuckDuckGo、必应中国、百度和 360。",
        ),
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": agent_text("Image search query.", "图片搜索关键词。") },
                "count": { "type": "integer", "description": agent_text("Required. Exact number of images to return. Match the user's requested quantity: one/a/an/一张/一幅 means 1; a few/几张 means 3; several/多张 means 5 unless the user gives another number. Do not use the configured maximum as the default.", "必填。最终返回图片的精确数量。必须匹配用户要求的数量：一张/一幅/one/a/an 填 1；几张填 3；多张填 5，除非用户给了其他数字。不要把配置上限当默认值。") },
                "preview": { "type": "boolean", "description": agent_text("Download and preview images with chafa when terminal image printing is enabled.", "在终端图片打印启用时，下载并用 chafa 预览图片。") },
                "preview_count": { "type": "integer", "description": agent_text("Maximum images to preview with chafa.", "最多用 chafa 预览几张图片。") },
                "safe_search": { "type": "boolean", "description": agent_text("Enable safe image search. Defaults to plugin config.", "启用安全搜图。默认使用插件配置。") }
            },
            "required": ["query", "count"],
            "additionalProperties": false
        }),
        move |args, progress| {
            let config = config.clone();
            let paths = paths.clone();
            async move { search_web_images(args, config, paths, allow_download, progress).await }
        },
    ));
}

pub(crate) async fn search_web_images(
    args: Value,
    config: AppConfig,
    paths: GQYPaths,
    allow_download: bool,
    progress: ToolProgress,
) -> Result<String> {
    let plugin = &config.plugins.web_images;
    if !plugin.enabled {
        bail!("web image search plugin is disabled")
    }
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if query.is_empty() {
        bail!("query is required")
    }
    let Some(count) = args.get("count").and_then(Value::as_u64) else {
        bail!("count is required; choose the number of images from the user's request")
    };
    let count = count.clamp(1, plugin.max_results.clamp(1, 10) as u64) as usize;
    let safe_search = args
        .get("safe_search")
        .and_then(Value::as_bool)
        .unwrap_or(plugin.safe_search)
        || plugin.safe_search;
    let preview = allow_download
        && args
            .get("preview")
            .and_then(Value::as_bool)
            .unwrap_or(plugin.auto_preview);
    let preview_count = args
        .get("preview_count")
        .and_then(Value::as_u64)
        .unwrap_or(count as u64)
        .clamp(0, count.min(5) as u64) as usize;
    let client = Client::builder()
        .timeout(Duration::from_secs(plugin.timeout_seconds.max(5)))
        .redirect(reqwest::redirect::Policy::limited(8))
        .build()?;
    progress.report(t("searching image candidates", "正在搜索图片候选"));
    let search = search_images(
        &client,
        &config,
        query,
        count,
        safe_search,
        allow_download && vision_screening_available(&config),
    )
    .await?;
    let candidates = search.candidates;
    if !allow_download {
        return Ok(json!({
            "success": !candidates.is_empty(),
            "query": query,
            "count": candidates.len().min(count),
            "mode": "metadata_only",
            "providers": search.diagnostics,
            "images": candidates.into_iter().take(count).map(candidate_json).collect::<Vec<_>>(),
        })
        .to_string());
    }
    let cache_dir = paths.pictures_dir.join("web-images");
    let download_result = download_and_store_images(
        &config,
        &paths,
        &cache_dir,
        query,
        candidates,
        count,
        configured_max_download_bytes(plugin.max_download_mb),
        progress.clone(),
    )
    .await?;
    let stored = download_result.images;
    for item in &stored {
        progress.report_image(item.local_path.clone(), item.candidate.title.clone());
    }
    let mut print_errors = Vec::new();
    let should_print = preview
        && config.plugins.print_image.enabled
        && preview_count > 0
        && progress.prepare_for_external_output().await;
    if should_print {
        for item in stored.iter().take(preview_count) {
            if let Err(err) = vision::print_image_file(
                &item.local_path,
                vision::configured_print_size(&config.plugins.print_image),
            )
            .await
            {
                print_errors.push(format!("{}: {err}", item.local_path.display()));
            }
        }
    }
    Ok(json!({
        "success": !stored.is_empty(),
        "query": query,
        "count": stored.len(),
        "result_role": "downloaded_image_candidates",
        "vision_screening": if vision_screening_available(&config) { "enabled" } else { "unavailable" },
        "description_policy": "vision.description is produced by the configured vision model after download; search_description is only search-engine metadata. Prefer vision.description when explaining whether an image matches the request.",
        "rejected_by_vision": download_result.rejected_by_vision,
        "providers": search.diagnostics,
        "cache_dir": cache_dir,
        "printed": should_print && print_errors.is_empty() && !stored.is_empty(),
        "print_errors": print_errors,
        "images": stored.into_iter().map(stored_json).collect::<Vec<_>>(),
        "assistant_instruction": if should_print {
            "The searched images have been downloaded and previewed in the terminal when possible. In your final response, include the local_path values for reusable images. Do not call print_image again for already printed images unless the user asks."
        } else {
            "The searched images have been downloaded to local_path. In your final response, include useful local_path and page_url values. Call print_image only if the user explicitly asks to render or preview them."
        }
    })
    .to_string())
}

pub(crate) fn configured_max_download_bytes(max_download_mb: f64) -> usize {
    let max_download_mb = if max_download_mb.is_nan() {
        0.1
    } else {
        max_download_mb.clamp(0.1, 50.0)
    };
    (max_download_mb * 1024.0 * 1024.0) as usize
}

pub(crate) struct DownloadResult {
    images: Vec<StoredImage>,
    rejected_by_vision: usize,
}

pub(crate) async fn search_images(
    client: &Client,
    config: &AppConfig,
    query: &str,
    count: usize,
    safe_search: bool,
    vision_safety_available: bool,
) -> Result<ImageSearchResult> {
    let limit = image_candidate_pool_limit(count);
    let all_providers = image_search_providers(config, query, safe_search, vision_safety_available);
    let mut diagnostics = Vec::new();
    let mut providers = all_providers
        .iter()
        .copied()
        .filter(provider_ready)
        .collect::<Vec<_>>();
    if providers.is_empty() {
        if let Some(provider) = provider_probe_candidate(&all_providers) {
            providers.push(provider);
        }
    } else {
        for provider in all_providers
            .iter()
            .copied()
            .filter(|provider| !providers.iter().any(|ready| ready.id() == provider.id()))
        {
            diagnostics.push(json!({
                "provider": provider.id(),
                "success": false,
                "skipped": "cooldown",
            }));
        }
    }
    let provider_timeout = Duration::from_secs(config.plugins.web_images.timeout_seconds.max(5));
    let searches = providers.into_iter().map(|provider| {
        let client = client.clone();
        let searxng_base_url = config.plugins.web.searxng_base_url.clone();
        let query = query.to_string();
        async move {
            let started = Instant::now();
            let result = tokio::time::timeout(
                provider_timeout,
                search_with_provider(
                    &client,
                    provider,
                    &searxng_base_url,
                    &query,
                    limit,
                    safe_search,
                ),
            )
            .await;
            let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            (provider, elapsed_ms, result)
        }
    });
    let mut candidates = Vec::new();
    for (provider, elapsed_ms, result) in join_all(searches).await {
        match result {
            Ok(Ok(mut items)) => {
                for (index, item) in items.iter_mut().enumerate() {
                    item.provider_rank = index + 1;
                }
                mark_provider_success(provider);
                diagnostics.push(json!({
                    "provider": provider.id(),
                    "success": true,
                    "elapsed_ms": elapsed_ms,
                    "candidates": items.len(),
                }));
                candidates.extend(items);
            }
            Ok(Err(err)) => {
                let message = err.to_string();
                mark_provider_failure(provider, &message);
                diagnostics.push(json!({
                    "provider": provider.id(),
                    "success": false,
                    "elapsed_ms": elapsed_ms,
                    "error": clean_text(&message, 240),
                }));
            }
            Err(_) => {
                mark_provider_failure(provider, "timeout");
                diagnostics.push(json!({
                    "provider": provider.id(),
                    "success": false,
                    "elapsed_ms": elapsed_ms,
                    "error": "provider timeout",
                }));
            }
        }
    }
    rank_candidates(query, &mut candidates);
    let candidates = dedupe_candidates(candidates);
    if candidates.is_empty() {
        bail!("image search returned no results")
    }
    Ok(ImageSearchResult {
        candidates: candidates.into_iter().take(limit).collect(),
        diagnostics,
    })
}

pub(crate) fn image_search_providers(
    config: &AppConfig,
    query: &str,
    safe_search: bool,
    vision_safety_available: bool,
) -> Vec<ImageSearchProvider> {
    let mode = config.plugins.web_images.source_mode.trim();
    let allow_best_effort_domestic = !safe_search || vision_safety_available;
    let mut providers = Vec::new();
    if !config.plugins.web.searxng_base_url.trim().is_empty() {
        providers.push(ImageSearchProvider::SearXng);
    }
    match mode {
        "mainland" => {
            providers.push(ImageSearchProvider::BingCn);
            if allow_best_effort_domestic {
                providers.extend([ImageSearchProvider::Baidu, ImageSearchProvider::So360]);
            }
        }
        "global" => {
            providers.extend([ImageSearchProvider::DuckDuckGo, ImageSearchProvider::BingCn])
        }
        _ if query.chars().any(is_cjk) => {
            providers.extend([ImageSearchProvider::DuckDuckGo, ImageSearchProvider::BingCn]);
            if allow_best_effort_domestic {
                providers.extend([ImageSearchProvider::Baidu, ImageSearchProvider::So360]);
            }
        }
        _ => providers.extend([ImageSearchProvider::DuckDuckGo, ImageSearchProvider::BingCn]),
    }
    providers
}

pub(crate) async fn search_with_provider(
    client: &Client,
    provider: ImageSearchProvider,
    searxng_base_url: &str,
    query: &str,
    limit: usize,
    safe_search: bool,
) -> Result<Vec<ImageCandidate>> {
    match provider {
        ImageSearchProvider::SearXng => {
            search_searxng_images(client, searxng_base_url, query, limit, safe_search).await
        }
        ImageSearchProvider::DuckDuckGo => {
            search_ddg_images(client, query, limit, safe_search).await
        }
        ImageSearchProvider::BingCn => search_bing_images(client, query, limit, safe_search).await,
        ImageSearchProvider::Baidu => search_baidu_images(client, query, limit).await,
        ImageSearchProvider::So360 => search_so360_images(client, query, limit).await,
    }
}

pub(crate) fn provider_ready(provider: &ImageSearchProvider) -> bool {
    let Ok(mut cooldowns) = PROVIDER_COOLDOWNS.lock() else {
        return true;
    };
    match cooldowns.get(provider.id()).copied() {
        Some(until) if until > Instant::now() => false,
        Some(_) => {
            cooldowns.remove(provider.id());
            true
        }
        None => true,
    }
}

pub(crate) fn provider_probe_candidate(
    providers: &[ImageSearchProvider],
) -> Option<ImageSearchProvider> {
    let cooldowns = PROVIDER_COOLDOWNS.lock().ok()?;
    providers.iter().copied().min_by_key(|provider| {
        cooldowns
            .get(provider.id())
            .copied()
            .unwrap_or(Instant::now())
    })
}

pub(crate) fn mark_provider_success(provider: ImageSearchProvider) {
    if let Ok(mut cooldowns) = PROVIDER_COOLDOWNS.lock() {
        cooldowns.remove(provider.id());
    }
}

pub(crate) fn mark_provider_failure(provider: ImageSearchProvider, error: &str) {
    let lower = error.to_ascii_lowercase();
    let duration = if lower.contains("403")
        || lower.contains("429")
        || lower.contains("forbid")
        || lower.contains("anti-bot")
        || lower.contains("captcha")
        || lower.contains("challenge")
    {
        Some(Duration::from_secs(600))
    } else if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection")
        || lower.contains("dns")
        || lower.contains("http 5")
    {
        Some(Duration::from_secs(120))
    } else {
        None
    };
    if let (Some(duration), Ok(mut cooldowns)) = (duration, PROVIDER_COOLDOWNS.lock()) {
        cooldowns.insert(provider.id(), Instant::now() + duration);
    }
}

pub(crate) async fn response_bytes_limited(response: reqwest::Response) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_SEARCH_RESPONSE_BYTES as u64)
    {
        bail!("image search response exceeds the 8 MiB limit")
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len().saturating_add(chunk.len()) > MAX_SEARCH_RESPONSE_BYTES {
            bail!("image search response exceeds the 8 MiB limit")
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) async fn response_json_limited(response: reqwest::Response) -> Result<Value> {
    Ok(serde_json::from_slice(
        &response_bytes_limited(response).await?,
    )?)
}

pub(crate) async fn response_text_limited(response: reqwest::Response) -> Result<String> {
    Ok(String::from_utf8_lossy(&response_bytes_limited(response).await?).into_owned())
}

pub(crate) async fn search_searxng_images(
    client: &Client,
    base_url: &str,
    query: &str,
    limit: usize,
    safe_search: bool,
) -> Result<Vec<ImageCandidate>> {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        bail!("missing SearXNG base URL")
    }
    let response = client
        .get(format!("{base_url}/search"))
        .query(&[
            ("q", query),
            ("categories", "images"),
            ("format", "json"),
            ("language", "auto"),
            ("safesearch", if safe_search { "2" } else { "0" }),
        ])
        .headers(image_headers(base_url))
        .send()
        .await?
        .error_for_status()?;
    let data = response_json_limited(response).await?;
    let mut candidates = Vec::new();
    for item in data
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(limit)
    {
        let (width, height) = parse_resolution(
            item.get("resolution")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        if let Some(candidate) = build_candidate(
            item.get("title")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            item.get("url").and_then(Value::as_str).unwrap_or_default(),
            item.get("img_src")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            item.get("thumbnail_src")
                .or_else(|| item.get("thumbnail"))
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "SearXNG Images",
            width as u64,
            height as u64,
            item.get("content")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ) {
            candidates.push(candidate);
        }
    }
    if candidates.is_empty() {
        bail!("SearXNG returned no image results")
    }
    Ok(candidates)
}

pub(crate) async fn search_baidu_images(
    client: &Client,
    query: &str,
    limit: usize,
) -> Result<Vec<ImageCandidate>> {
    let response = client
        .get("https://image.baidu.com/search/acjson")
        .query(&[
            ("tn", "resultjson_com"),
            ("ipn", "rj"),
            ("ct", "201326592"),
            ("fp", "result"),
            ("word", query),
            ("queryWord", query),
            ("cl", "2"),
            ("lm", "-1"),
            ("ie", "utf-8"),
            ("oe", "utf-8"),
            ("st", "-1"),
            ("face", "0"),
            ("istype", "2"),
            ("nc", "1"),
            ("pn", "0"),
            ("rn", &limit.min(60).to_string()),
        ])
        .headers(image_headers("https://image.baidu.com/"))
        .send()
        .await?
        .error_for_status()?;
    let data = response_json_limited(response).await?;
    if data.get("antiFlag").is_some() {
        bail!("Baidu Images anti-bot response")
    }
    let mut candidates = Vec::new();
    for item in data
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(limit)
    {
        let replacement = item
            .get("replaceUrl")
            .and_then(Value::as_array)
            .and_then(|items| items.first());
        let image_url = replacement
            .and_then(|value| value.get("ObjURL").or_else(|| value.get("ObjUrl")))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .or_else(|| item.get("middleURL").and_then(Value::as_str))
            .unwrap_or_default();
        let page_url = replacement
            .and_then(|value| value.get("FromURL").or_else(|| value.get("FromUrl")))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .or_else(|| item.get("fromJumpUrl").and_then(Value::as_str))
            .unwrap_or_default();
        if let Some(candidate) = build_candidate(
            item.get("fromPageTitleEnc")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            page_url,
            image_url,
            item.get("thumbURL")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "Baidu Images",
            item.get("width").and_then(Value::as_u64).unwrap_or(0),
            item.get("height").and_then(Value::as_u64).unwrap_or(0),
            "",
        ) {
            candidates.push(candidate);
        }
    }
    if candidates.is_empty() {
        bail!("Baidu Images returned no results")
    }
    Ok(candidates)
}

pub(crate) async fn search_so360_images(
    client: &Client,
    query: &str,
    limit: usize,
) -> Result<Vec<ImageCandidate>> {
    let response = client
        .get("https://image.so.com/j")
        .query(&[
            ("q", query),
            ("src", "srp"),
            ("sn", "0"),
            ("pn", &limit.min(60).to_string()),
        ])
        .headers(image_headers("https://image.so.com/"))
        .send()
        .await?
        .error_for_status()?;
    let data = response_json_limited(response).await?;
    let mut candidates = Vec::new();
    for item in data
        .get("list")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(limit)
    {
        if let Some(candidate) = build_candidate(
            item.get("title")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            item.get("link").and_then(Value::as_str).unwrap_or_default(),
            item.get("img").and_then(Value::as_str).unwrap_or_default(),
            item.get("thumb")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "360 Images",
            parse_u64ish(item.get("width")),
            parse_u64ish(item.get("height")),
            item.get("dspurl")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ) {
            candidates.push(candidate);
        }
    }
    if candidates.is_empty() {
        bail!("360 Images returned no results")
    }
    Ok(candidates)
}

pub(crate) fn parse_u64ish(value: Option<&Value>) -> u64 {
    value
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .unwrap_or(0)
}

pub(crate) fn parse_resolution(value: &str) -> (u32, u32) {
    let values = value
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|value| !value.is_empty())
        .filter_map(|value| value.parse::<u32>().ok())
        .take(2)
        .collect::<Vec<_>>();
    match values.as_slice() {
        [width, height] => (*width, *height),
        _ => (0, 0),
    }
}

pub(crate) async fn search_ddg_images(
    client: &Client,
    query: &str,
    limit: usize,
    safe_search: bool,
) -> Result<Vec<ImageCandidate>> {
    let page_url = format!(
        "https://duckduckgo.com/?q={}&iax=images&ia=images",
        urlencoding::encode(query)
    );
    let page_response = client
        .get("https://duckduckgo.com/")
        .query(&[("q", query), ("iax", "images"), ("ia", "images")])
        .headers(image_headers(""))
        .send()
        .await?;
    let page_status = page_response.status();
    let html = response_text_limited(page_response).await?;
    if page_status.as_u16() != 200 || looks_like_search_challenge(&html) {
        bail!("DuckDuckGo image challenge or HTTP {page_status}")
    }
    let vqd = extract_ddg_vqd(&html).context("DuckDuckGo image page did not return vqd")?;
    let api_response = client
        .get("https://duckduckgo.com/i.js")
        .query(&[
            ("q", query),
            ("o", "json"),
            ("p", if safe_search { "1" } else { "-1" }),
            ("s", "0"),
            ("u", "bing"),
            ("f", ",,,"),
            (
                "l",
                if query.chars().any(is_cjk) {
                    "cn-zh"
                } else {
                    "wt-wt"
                },
            ),
            ("vqd", vqd.as_str()),
        ])
        .headers(image_headers(&page_url))
        .send()
        .await?;
    let api_status = api_response.status();
    let response = response_text_limited(api_response).await?;
    if api_status.as_u16() != 200 || looks_like_search_challenge(&response) {
        bail!("DuckDuckGo image API challenge or HTTP {api_status}")
    }
    parse_ddg_results(&response, limit)
}

pub(crate) fn extract_ddg_vqd(html: &str) -> Option<String> {
    for marker in ["vqd=\"", "vqd='", "vqd:\"", "vqd: '"] {
        if let Some(start) = html.find(marker) {
            let rest = &html[start + marker.len()..];
            let value: String = rest
                .chars()
                .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
                .collect();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    if let Some(start) = html.find("\"vqd\":\"") {
        let rest = &html[start + "\"vqd\":\"".len()..];
        let value: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
            .collect();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

pub(crate) fn parse_ddg_results(text: &str, limit: usize) -> Result<Vec<ImageCandidate>> {
    let data: Value = serde_json::from_str(text)?;
    let results = data
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut candidates = Vec::new();
    for item in results.into_iter().take(limit) {
        if let Some(candidate) = build_candidate(
            item.get("title")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            item.get("url").and_then(Value::as_str).unwrap_or_default(),
            item.get("image")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            item.get("thumbnail")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "DuckDuckGo Images",
            item.get("width").and_then(Value::as_u64).unwrap_or(0),
            item.get("height").and_then(Value::as_u64).unwrap_or(0),
            "",
        ) {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

pub(crate) async fn search_bing_images(
    client: &Client,
    query: &str,
    limit: usize,
    safe_search: bool,
) -> Result<Vec<ImageCandidate>> {
    let mut request = client
        .get("https://cn.bing.com/images/search")
        .query(&[("q", query), ("first", "1"), ("mkt", "zh-CN")])
        .headers(image_headers(""));
    if safe_search {
        request = request.query(&[("safeSearch", "Strict")]);
    }
    let html = response_text_limited(request.send().await?.error_for_status()?).await?;
    let candidates = parse_bing_results(&html, limit);
    if candidates.is_empty() {
        bail!("Bing CN Images returned no parseable results")
    }
    Ok(candidates)
}

pub(crate) fn parse_bing_results(html: &str, limit: usize) -> Vec<ImageCandidate> {
    let mut candidates = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("<a") {
        rest = &rest[pos..];
        let Some(iusc_pos) = rest.find("class=\"iusc\"") else {
            if rest.len() <= 2 {
                break;
            }
            rest = &rest[2..];
            continue;
        };
        rest = &rest[iusc_pos..];
        let Some(m_pos) = rest.find("m=\"") else {
            rest = &rest[1..];
            continue;
        };
        let start = m_pos + 3;
        let Some(end) = rest[start..].find('"') else {
            break;
        };
        let raw = html_unescape(&rest[start..start + end]);
        if let Ok(data) = serde_json::from_str::<Value>(&raw) {
            if let Some(candidate) = build_candidate(
                data.get("t")
                    .or_else(|| data.get("desc"))
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                data.get("purl").and_then(Value::as_str).unwrap_or_default(),
                data.get("murl").and_then(Value::as_str).unwrap_or_default(),
                data.get("turl").and_then(Value::as_str).unwrap_or_default(),
                "Bing CN Images",
                data.get("w")
                    .or_else(|| data.get("expw"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                data.get("h")
                    .or_else(|| data.get("exph"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                data.get("desc").and_then(Value::as_str).unwrap_or_default(),
            ) {
                candidates.push(candidate);
            }
        }
        if candidates.len() >= limit {
            break;
        }
        rest = &rest[start + end..];
    }
    candidates
}

pub(crate) fn build_candidate(
    title: &str,
    page_url: &str,
    image_url: &str,
    thumbnail_url: &str,
    source: &str,
    width: u64,
    height: u64,
    extra_description: &str,
) -> Option<ImageCandidate> {
    let image_url = clean_url(image_url);
    if !image_url.starts_with("http://") && !image_url.starts_with("https://") {
        return None;
    }
    let title = clean_text(title, 180);
    let page_url = clean_url(page_url);
    let thumbnail_url = clean_url(thumbnail_url);
    let mut description_parts = vec![title.clone(), clean_text(extra_description, 180)];
    if let Some(host) = host_from_url(&page_url) {
        description_parts.push(format!("来源页面: {host}"));
    }
    let search_description = clean_text(
        &description_parts
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("；"),
        420,
    );
    Some(ImageCandidate {
        title,
        page_url,
        image_url,
        thumbnail_url,
        source: source.to_string(),
        width: width.min(u32::MAX as u64) as u32,
        height: height.min(u32::MAX as u64) as u32,
        search_description,
        provider_rank: 0,
    })
}

pub(crate) async fn download_and_store_images(
    config: &AppConfig,
    paths: &GQYPaths,
    cache_dir: &Path,
    query: &str,
    candidates: Vec<ImageCandidate>,
    count: usize,
    max_bytes: usize,
    progress: ToolProgress,
) -> Result<DownloadResult> {
    tokio::fs::create_dir_all(cache_dir)
        .await
        .with_context(|| format!("failed to create {}", cache_dir.display()))?;
    let call_temp_dir = CallTempDir::new(cache_dir)?;
    let mut completed = Vec::new();
    let mut download_error = None;
    let probe_limit = image_download_probe_limit(count);
    let download_timeout = Duration::from_secs(config.plugins.web_images.timeout_seconds.max(5));
    let downloads =
        candidates
            .into_iter()
            .take(probe_limit)
            .enumerate()
            .map(|(index, candidate)| {
                let temp_dir = call_temp_dir.path().to_path_buf();
                async move {
                    (
                        index,
                        download_candidate(
                            &temp_dir,
                            index,
                            candidate,
                            max_bytes,
                            download_timeout,
                        )
                        .await,
                    )
                }
            });
    let mut downloads =
        futures_util::stream::iter(downloads).buffer_unordered(probe_limit.clamp(1, 4));
    while let Some((index, result)) = downloads.next().await {
        progress.report(format!(
            "{} {}/{}",
            t("downloading images", "正在下载图片"),
            completed.len() + 1,
            probe_limit
        ));
        let result = match result {
            Ok(result) => result,
            Err(err) => {
                download_error.get_or_insert(err);
                continue;
            }
        };
        let Some(mut item) = result else {
            continue;
        };
        item.vision = VisionScreening::not_requested();
        completed.push((index, item));
    }
    if let Some(err) = download_error {
        return Err(err);
    }
    let mut downloaded = dedupe_downloaded(completed);
    if downloaded.is_empty() {
        bail!("image search found candidates, but no image could be downloaded")
    }
    if vision_screening_available(config) {
        progress.report(t("reviewing images", "正在批量审核图片"));
        screen_images_with_vision(config, paths, query, &mut downloaded).await;
    }
    let (mut stored, rejected_by_vision) = select_images(query, downloaded, count);
    if stored.is_empty() {
        bail!("image search candidates were unavailable or rejected by safety review")
    }
    for item in &mut stored {
        publish_image(cache_dir, item).await?;
    }
    progress.report(format!(
        "{} {}/{}",
        t("accepted images", "已通过图片"),
        stored.len(),
        count
    ));
    Ok(DownloadResult {
        images: stored,
        rejected_by_vision,
    })
}

pub(crate) fn dedupe_downloaded(mut completed: Vec<(usize, StoredImage)>) -> Vec<StoredImage> {
    completed.sort_by_key(|(index, _)| *index);
    let mut seen_hashes = HashSet::new();
    completed
        .into_iter()
        .filter_map(|(_, item)| seen_hashes.insert(item.sha256.clone()).then_some(item))
        .collect()
}

pub(crate) fn select_images(
    query: &str,
    downloaded: Vec<StoredImage>,
    count: usize,
) -> (Vec<StoredImage>, usize) {
    let before_filter = downloaded.len();
    let mut stored = Vec::new();
    for item in downloaded {
        if item.vision.accepted && item.vision.safe {
            stored.push(item);
        }
    }
    let rejected_by_vision = before_filter.saturating_sub(stored.len());
    stored.sort_by(|left, right| {
        right
            .vision
            .relevance
            .cmp(&left.vision.relevance)
            .then_with(|| right.vision.quality.cmp(&left.vision.quality))
            .then_with(|| {
                score_candidate(query, &right.candidate)
                    .partial_cmp(&score_candidate(query, &left.candidate))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    stored.truncate(count);
    (stored, rejected_by_vision)
}

pub(crate) async fn download_candidate(
    temp_dir: &Path,
    candidate_index: usize,
    mut candidate: ImageCandidate,
    max_bytes: usize,
    timeout: Duration,
) -> Result<Option<StoredImage>> {
    let urls =
        if candidate.thumbnail_url.is_empty() || candidate.thumbnail_url == candidate.image_url {
            vec![(candidate.image_url.clone(), false)]
        } else {
            vec![
                (candidate.image_url.clone(), false),
                (candidate.thumbnail_url.clone(), true),
            ]
        };
    for (url, used_thumbnail) in urls {
        let deadline = Instant::now() + timeout;
        let Ok((bytes, final_url, content_type)) =
            download_image_bytes(&url, &candidate.page_url, max_bytes, deadline).await
        else {
            continue;
        };
        let decode_permit = IMAGE_DECODE_PERMITS
            .clone()
            .acquire_owned()
            .await
            .context("web image decode limiter closed")?;
        let validated = match tokio::task::spawn_blocking(move || {
            let _decode_permit = decode_permit;
            validate_downloaded_image(bytes, content_type, final_url)
        })
        .await
        {
            Ok(Some(validated)) => validated,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(error = %error, "web image decoder task failed");
                continue;
            }
        };
        let ValidatedImage {
            bytes,
            mime_type,
            width,
            height,
            sha256,
        } = validated;
        let ext = extension_for_mime(&mime_type);
        let local_path = temp_dir.join(format!("candidate-{candidate_index}-{sha256}{ext}"));
        if let Err(error) = write_temp_file(&local_path, &bytes).await {
            tracing::warn!(
                error = %error,
                path = %local_path.display(),
                "failed to stage web image candidate"
            );
            continue;
        }
        if width > 0 && height > 0 {
            candidate.width = width;
            candidate.height = height;
        }
        return Ok(Some(StoredImage {
            candidate,
            local_path,
            mime_type,
            size_bytes: bytes.len(),
            sha256,
            used_thumbnail,
            vision: VisionScreening::not_requested(),
        }));
    }
    Ok(None)
}

pub(crate) struct ValidatedImage {
    bytes: Vec<u8>,
    mime_type: String,
    width: u32,
    height: u32,
    sha256: String,
}

pub(crate) fn validate_downloaded_image(
    bytes: Vec<u8>,
    content_type: String,
    final_url: String,
) -> Option<ValidatedImage> {
    let mime_type = detect_image_mime(&bytes, &content_type, &final_url)?;
    let (width, height) = detect_image_dimensions(&bytes, &mime_type);
    if !image_dimensions_allowed(width, height) {
        return None;
    }
    let mut reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    limits.max_alloc = Some(IMAGE_DECODER_MAX_ALLOC);
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    if decoded.dimensions() != (width, height) {
        return None;
    }
    let sha256 = hex::encode(Sha256::digest(&bytes));
    Some(ValidatedImage {
        bytes,
        mime_type,
        width,
        height,
        sha256,
    })
}

pub(crate) fn image_dimensions_allowed(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && u64::from(width).saturating_mul(u64::from(height)) <= MAX_IMAGE_PIXELS
}

pub(crate) async fn write_temp_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
    {
        Ok(file) => file,
        Err(err) => {
            return Err(err).with_context(|| format!("failed to create {}", path.display()))
        }
    };
    let write_result = async {
        file.write_all(bytes).await?;
        file.flush().await?;
        file.sync_all().await
    }
    .await;
    if let Err(err) = write_result {
        drop(file);
        if let Err(cleanup_error) = tokio::fs::remove_file(path).await {
            if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    error = %cleanup_error,
                    path = %path.display(),
                    "failed to remove incomplete web image temp file"
                );
            }
        }
        return Err(err).with_context(|| format!("failed to write {}", path.display()));
    }
    Ok(())
}

pub(crate) async fn publish_image(cache_dir: &Path, item: &mut StoredImage) -> Result<()> {
    let final_path = cache_dir.join(format!(
        "webimg-{}{}",
        item.sha256,
        extension_for_mime(&item.mime_type)
    ));
    let _publish_guard = CACHE_PUBLISH_LOCK.lock().await;
    let source = item.local_path.clone();
    let expected_hash = item.sha256.clone();
    let expected_size = item.size_bytes;
    let cache_dir = cache_dir.to_path_buf();
    let publish_path = final_path.clone();
    tokio::task::spawn_blocking(move || {
        publish_cache_file(
            &source,
            &publish_path,
            &cache_dir,
            &expected_hash,
            expected_size,
        )
    })
    .await
    .context("web image cache publish task failed")??;
    item.local_path = final_path;
    Ok(())
}

pub(crate) fn publish_cache_file(
    source: &Path,
    final_path: &Path,
    cache_dir: &Path,
    expected_hash: &str,
    expected_size: usize,
) -> Result<()> {
    for _ in 0..8 {
        match std::fs::hard_link(source, final_path) {
            Ok(()) => {
                // The hard link is already committed and cannot be rolled back safely. A failed
                // directory sync is therefore reported, but must not remove the shared cache.
                if let Err(error) = std::fs::File::open(cache_dir).and_then(|file| file.sync_all())
                {
                    tracing::warn!(
                        error = %error,
                        path = %cache_dir.display(),
                        "web image cache published but directory sync failed"
                    );
                }
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = match std::fs::symlink_metadata(final_path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("failed to inspect existing {}", final_path.display())
                        })
                    }
                };
                if metadata.file_type().is_dir() {
                    bail!(
                        "web image cache path is a directory: {}",
                        final_path.display()
                    )
                }
                if metadata.file_type().is_symlink() {
                    remove_invalid_cache_entry(final_path)?;
                    continue;
                }
                if !metadata.file_type().is_file() {
                    bail!(
                        "web image cache path is not a regular file: {}",
                        final_path.display()
                    )
                }
                if valid_cached_file(final_path, expected_hash, expected_size)? {
                    return Ok(());
                }
                remove_invalid_cache_entry(final_path)?;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to publish {}", final_path.display()))
            }
        }
    }
    bail!("could not publish web image without replacing a concurrent cache entry")
}

pub(crate) fn remove_invalid_cache_entry(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to remove invalid {}", path.display()))
        }
    }
}
