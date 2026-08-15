//! crawlers — 自 src/tools/web.rs 拆分。

use super::*;

pub(crate) async fn search_duckduckgo(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Result<String> {
    if is_ddg_blocked() {
        let fallback = search_fallback_html(client, query, max_results).await;
        if !fallback.is_empty() {
            return Ok(format_crawler_results(
                query,
                "DuckDuckGo (via fallback)",
                fallback,
            ));
        }
        bail!("DuckDuckGo is blocked by captcha and fallback engines returned no results");
    }

    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    );
    let response = client
        .get(&url)
        .header("User-Agent", CRAWLER_USER_AGENT)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await;

    let html = match response {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            if looks_like_ddg_challenge(status, &text) {
                set_ddg_blocked(Duration::from_secs(60));
                let fallback = search_fallback_html(client, query, max_results).await;
                if !fallback.is_empty() {
                    return Ok(format_crawler_results(
                        query,
                        "DuckDuckGo (via fallback - DDG captcha)",
                        fallback,
                    ));
                }
                bail!(
                    "DuckDuckGo returned a captcha page and fallback engines returned no results"
                );
            }
            if status != 200 {
                let fallback = search_fallback_html(client, query, max_results).await;
                if !fallback.is_empty() {
                    return Ok(format_crawler_results(
                        query,
                        "DuckDuckGo (via fallback - DDG HTTP error)",
                        fallback,
                    ));
                }
                bail!("DuckDuckGo HTTP {status} and fallback returned no results");
            }
            text
        }
        Err(_) => {
            let fallback = search_fallback_html(client, query, max_results).await;
            if !fallback.is_empty() {
                return Ok(format_crawler_results(
                    query,
                    "DuckDuckGo (via fallback - DDG request failed)",
                    fallback,
                ));
            }
            bail!("DuckDuckGo request failed and fallback returned no results");
        }
    };

    let results = parse_duckduckgo_html(&html, max_results);
    if !results.is_empty() {
        return Ok(format_crawler_results(query, "DuckDuckGo HTML", results));
    }

    let fallback = search_fallback_html(client, query, max_results).await;
    if !fallback.is_empty() {
        return Ok(format_crawler_results(
            query,
            "DuckDuckGo (via fallback - DDG no results)",
            fallback,
        ));
    }
    bail!("DuckDuckGo returned no parseable results and fallback returned no results");
}

pub(crate) fn parse_duckduckgo_html(html: &str, max_results: usize) -> Vec<CrawlerResult> {
    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut rest = html;
    while let Some(link_pos) = rest.find("result__a") {
        rest = &rest[link_pos..];
        let Some(href_pos) = rest.find("href=\"") else {
            break;
        };
        let href_start = href_pos + "href=\"".len();
        let Some(href_end) = rest[href_start..].find('"') else {
            break;
        };
        let raw_url = unwrap_ddg_url(&rest[href_start..href_start + href_end]);
        let Some(tag_end) = rest[href_start + href_end..].find('>') else {
            break;
        };
        let title_start = href_start + href_end + tag_end + 1;
        let Some(title_end) = rest[title_start..].find("</a>") else {
            break;
        };
        let title = clean_html_text(&rest[title_start..title_start + title_end]);
        let snippet =
            if let Some(snippet_pos) = rest[title_start + title_end..].find("result__snippet") {
                let snippet_rest = &rest[title_start + title_end + snippet_pos..];
                if let Some(open_end) = snippet_rest.find('>') {
                    if let Some(close) = snippet_rest[open_end + 1..].find("</") {
                        clean_html_text(&snippet_rest[open_end + 1..open_end + 1 + close])
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
        if !title.is_empty() && !raw_url.is_empty() && is_result_url_allowed(&raw_url) {
            let key = dedupe_key(&raw_url);
            if seen.insert(key) {
                results.push(CrawlerResult {
                    title,
                    url: raw_url,
                    snippet,
                    source: "DuckDuckGo".to_string(),
                });
            }
        }
        if results.len() >= max_results {
            break;
        }
        rest = &rest[title_start + title_end..];
    }
    results
}

// ── Yahoo HTML search ──────────────────────────────────────────

pub(crate) async fn search_yahoo_html(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    let url = format!(
        "https://search.yahoo.com/search?p={}",
        urlencoding::encode(query)
    );
    let html = match client
        .get(&url)
        .header("User-Agent", CRAWLER_USER_AGENT)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status().as_u16() != 200 {
                return Vec::new();
            }
            resp.text().await.unwrap_or_default()
        }
        Err(_) => return Vec::new(),
    };
    parse_yahoo_html(&html, max_results)
}

pub(crate) fn parse_yahoo_html(html: &str, max_results: usize) -> Vec<CrawlerResult> {
    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut rest = html;
    while let Some(pos) = rest.find("class=\"dd algo") {
        rest = &rest[pos..];
        let anchor_start = match rest.find("href=\"") {
            Some(p) => p + "href=\"".len(),
            None => {
                rest = &rest[10..];
                continue;
            }
        };
        let Some(href_end) = rest[anchor_start..].find('"') else {
            break;
        };
        let raw_url = unwrap_yahoo_url(&rest[anchor_start..anchor_start + href_end]);
        let Some(tag_end) = rest[anchor_start + href_end..].find('>') else {
            rest = &rest[anchor_start + href_end..];
            continue;
        };
        let title_start = anchor_start + href_end + tag_end + 1;
        let Some(title_end) = rest[title_start..].find("</a>") else {
            break;
        };
        let title = clean_html_text(&rest[title_start..title_start + title_end]);
        let snippet = extract_snippet_after(&rest[title_start + title_end..], "compText")
            .or_else(|| extract_snippet_after(&rest[title_start + title_end..], "<p"))
            .unwrap_or_default();
        if !title.is_empty() && !raw_url.is_empty() && is_result_url_allowed(&raw_url) {
            let key = dedupe_key(&raw_url);
            if seen.insert(key) {
                results.push(CrawlerResult {
                    title,
                    url: raw_url,
                    snippet,
                    source: "Yahoo".to_string(),
                });
            }
        }
        if results.len() >= max_results {
            break;
        }
        rest = &rest[title_start + title_end..];
    }
    results
}

// ── 360 (so.com) HTML search ───────────────────────────────────

pub(crate) async fn search_so_html(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    let url = format!("https://www.so.com/s?q={}", urlencoding::encode(query));
    let html = match client
        .get(&url)
        .header("User-Agent", CRAWLER_USER_AGENT)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status().as_u16() != 200 {
                return Vec::new();
            }
            resp.text().await.unwrap_or_default()
        }
        Err(_) => return Vec::new(),
    };
    parse_so_html(client, &html, max_results).await
}

pub(crate) async fn parse_so_html(
    client: &reqwest::Client,
    html: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    let mut candidates: Vec<(String, String, String)> = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("class=\"result") {
        rest = &rest[pos..];
        let h3_pos = match rest.find("<h3") {
            Some(p) => p,
            None => {
                rest = &rest[10..];
                continue;
            }
        };
        let h3_rest = &rest[h3_pos..];
        let href_start = match h3_rest.find("href=\"") {
            Some(p) => p + "href=\"".len(),
            None => {
                rest = &rest[h3_pos + 3..];
                continue;
            }
        };
        let Some(href_end) = h3_rest[href_start..].find('"') else {
            break;
        };
        let href = html_unescape(&h3_rest[href_start..href_start + href_end]);
        let Some(tag_end) = h3_rest[href_start + href_end..].find('>') else {
            rest = &rest[h3_pos + 3..];
            continue;
        };
        let title_start = href_start + href_end + tag_end + 1;
        let Some(title_end) = h3_rest[title_start..].find("</a>") else {
            break;
        };
        let title = clean_html_text(&h3_rest[title_start..title_start + title_end]);
        let snippet = extract_snippet_after(&h3_rest[title_start + title_end..], "res-desc")
            .or_else(|| extract_snippet_after(&h3_rest[title_start + title_end..], "fz-mid"))
            .or_else(|| extract_snippet_after(&h3_rest[title_start + title_end..], "<p"))
            .unwrap_or_default();
        if !title.is_empty() && !href.is_empty() {
            candidates.push((title, href, snippet));
        }
        if candidates.len() >= max_results * 2 {
            break;
        }
        rest = &h3_rest[title_start + title_end..];
    }

    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (title, href, snippet) in candidates {
        if results.len() >= max_results {
            break;
        }
        let resolved = resolve_so_url(client, &href).await;
        if !resolved.is_empty() && is_result_url_allowed(&resolved) {
            let key = dedupe_key(&resolved);
            if seen.insert(key) {
                results.push(CrawlerResult {
                    title,
                    url: resolved,
                    snippet,
                    source: "360".to_string(),
                });
            }
        }
    }
    results
}

pub(crate) async fn resolve_so_url(client: &reqwest::Client, href: &str) -> String {
    let href = html_unescape(href.trim());
    if href.is_empty() {
        return String::new();
    }
    let absolute = if href.starts_with("http://") || href.starts_with("https://") {
        href.clone()
    } else {
        format!("https://www.so.com{}", href)
    };
    if !(absolute.contains("so.com") && absolute.contains("/link")) {
        return absolute;
    }
    match client.get(&absolute).send().await {
        Ok(resp) => {
            let final_url = resp.url().to_string();
            if final_url != absolute
                && (final_url.starts_with("http://") || final_url.starts_with("https://"))
            {
                return final_url;
            }
            let text = resp.text().await.unwrap_or_default();
            if let Some(pos) = text.find("window.location") {
                let rest = &text[pos..];
                if let Some(q1) = rest.find('"') {
                    if let Some(q2) = rest[q1 + 1..].find('"') {
                        return html_unescape(&rest[q1 + 1..q1 + 1 + q2]);
                    }
                }
            }
            if let Some(pos) = text.find("URL=") {
                let rest = &text[pos + 4..];
                let end = rest
                    .find('"')
                    .or_else(|| rest.find('>'))
                    .unwrap_or(rest.len());
                let url_str = rest[..end].trim_matches('\'');
                return html_unescape(url_str);
            }
            absolute
        }
        Err(_) => absolute,
    }
}

// ── Sogou HTML search ──────────────────────────────────────────

pub(crate) async fn search_sogou_html(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    if is_sogou_blocked() {
        return Vec::new();
    }
    let url = format!(
        "https://www.sogou.com/web?query={}&ie=utf8",
        urlencoding::encode(query)
    );
    let html = match client
        .get(&url)
        .header("User-Agent", CRAWLER_USER_AGENT)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await
    {
        Ok(resp) => {
            let final_url = resp.url().to_string();
            let text = resp.text().await.unwrap_or_default();
            if final_url.contains("antispider")
                || text.contains("SourceVerifyCode")
                || text.contains("\u{6b64}\u{9a8c}\u{8bc1}\u{7801}\u{7528}\u{4e8e}\u{786e}\u{8ba4}")
            {
                set_sogou_blocked(Duration::from_secs(300));
                return Vec::new();
            }
            text
        }
        Err(_) => return Vec::new(),
    };
    parse_sogou_html(client, &html, max_results).await
}

pub(crate) async fn parse_sogou_html(
    client: &reqwest::Client,
    html: &str,
    max_results: usize,
) -> Vec<CrawlerResult> {
    let mut candidates: Vec<(String, String, String)> = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("class=\"vrwrap") {
        rest = &rest[pos..];
        let h3_pos = match rest.find("<h3") {
            Some(p) => p,
            None => {
                rest = &rest[10..];
                continue;
            }
        };
        let h3_rest = &rest[h3_pos..];
        let href_start = match h3_rest.find("href=\"") {
            Some(p) => p + "href=\"".len(),
            None => {
                rest = &rest[h3_pos + 3..];
                continue;
            }
        };
        let Some(href_end) = h3_rest[href_start..].find('"') else {
            break;
        };
        let href = html_unescape(&h3_rest[href_start..href_start + href_end]);
        let Some(tag_end) = h3_rest[href_start + href_end..].find('>') else {
            rest = &rest[h3_pos + 3..];
            continue;
        };
        let title_start = href_start + href_end + tag_end + 1;
        let Some(title_end) = h3_rest[title_start..].find("</a>") else {
            break;
        };
        let title = clean_html_text(&h3_rest[title_start..title_start + title_end]);
        let snippet = extract_snippet_after(&h3_rest[title_start + title_end..], "fz-mid")
            .or_else(|| extract_snippet_after(&h3_rest[title_start + title_end..], "str_info"))
            .or_else(|| extract_snippet_after(&h3_rest[title_start + title_end..], "<p"))
            .unwrap_or_default();
        if !title.is_empty() && !href.is_empty() {
            candidates.push((title, href, snippet));
        }
        if candidates.len() >= max_results * 2 {
            break;
        }
        rest = &h3_rest[title_start + title_end..];
    }

    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (title, href, snippet) in candidates {
        if results.len() >= max_results {
            break;
        }
        let resolved = resolve_sogou_url(client, &href).await;
        if !resolved.is_empty() && is_result_url_allowed(&resolved) {
            let key = dedupe_key(&resolved);
            if seen.insert(key) {
                results.push(CrawlerResult {
                    title,
                    url: resolved,
                    snippet,
                    source: "Sogou".to_string(),
                });
            }
        }
    }
    results
}

pub(crate) async fn resolve_sogou_url(client: &reqwest::Client, href: &str) -> String {
    let href = html_unescape(href.trim());
    if href.is_empty() {
        return String::new();
    }
    let absolute = if href.starts_with("http://") || href.starts_with("https://") {
        href.clone()
    } else {
        format!("https://www.sogou.com{}", href)
    };
    if !(absolute.contains("sogou.com") && absolute.contains("/link")) {
        return absolute;
    }
    match client.get(&absolute).send().await {
        Ok(resp) => {
            let final_url = resp.url().to_string();
            if final_url != absolute
                && (final_url.starts_with("http://") || final_url.starts_with("https://"))
            {
                return final_url;
            }
            let text = resp.text().await.unwrap_or_default();
            if let Some(pos) = text.find("window.location") {
                let rest = &text[pos..];
                if let Some(q1) = rest.find('"') {
                    if let Some(q2) = rest[q1 + 1..].find('"') {
                        return html_unescape(&rest[q1 + 1..q1 + 1 + q2]);
                    }
                }
            }
            if let Some(pos) = text.find("URL=") {
                let rest = &text[pos + 4..];
                let end = rest
                    .find('"')
                    .or_else(|| rest.find('>'))
                    .unwrap_or(rest.len());
                let url_str = rest[..end].trim_matches('\'');
                return html_unescape(url_str);
            }
            absolute
        }
        Err(_) => absolute,
    }
}

// ── Multi-engine fallback dispatcher ────────────────────────────
