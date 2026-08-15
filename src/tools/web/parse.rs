//! parse — 自 src/tools/web.rs 拆分。

use super::*;

pub(crate) fn clean_html_text(value: &str) -> String {
    html_unescape(&html_conversion::to_text_lossy(value, 120))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn html_unescape(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

pub(crate) fn firecrawl_results(data: &Value, max_results: usize) -> Vec<Value> {
    let data_value = data.get("data").unwrap_or(data);
    let results = data_value
        .as_array()
        .or_else(|| data_value.get("web").and_then(Value::as_array))
        .or_else(|| data_value.get("results").and_then(Value::as_array));
    results
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(max_results)
        .collect()
}

pub(crate) fn anysearch_results(data: &Value, max_results: usize) -> Vec<Value> {
    let results = data
        .get("results")
        .and_then(Value::as_array)
        .or_else(|| data.pointer("/data/results").and_then(Value::as_array));
    results
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(max_results)
        .collect()
}

pub(crate) fn format_search_results(query: &str, provider: &str, results: Vec<Value>) -> Result<String> {
    let mut lines = vec![
        format!("## Search results for: {query}"),
        format!("**Provider**: {provider}\n"),
    ];
    let mut rendered = 0usize;
    for item in results.into_iter() {
        let title = item
            .get("title")
            .or_else(|| item.pointer("/metadata/title"))
            .and_then(Value::as_str)
            .unwrap_or("Untitled");
        let url = item
            .get("url")
            .or_else(|| item.pointer("/metadata/sourceURL"))
            .or_else(|| item.pointer("/metadata/url"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let snippet = item
            .get("content")
            .or_else(|| item.get("snippet"))
            .or_else(|| item.get("description"))
            .or_else(|| item.get("summary"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let raw = item
            .get("raw_content")
            .or_else(|| item.get("markdown"))
            .or_else(|| item.get("contentMarkdown"))
            .or_else(|| item.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if title == "Untitled" && url.is_empty() && snippet.is_empty() && raw.is_empty() {
            continue;
        }
        rendered += 1;
        lines.push(format!("### {}. {title}", rendered));
        if !url.is_empty() {
            lines.push(format!("**URL**: {url}"));
        }
        if !snippet.is_empty() {
            lines.push(format!("**Snippet**: {}", clip(snippet, 500)));
        }
        if !raw.is_empty() {
            lines.push(format!("**Content**: {}", clip(raw, 800)));
        }
        lines.push(String::new());
    }
    if rendered == 0 {
        bail!("{provider} returned no usable results")
    }
    Ok(lines.join("\n"))
}

pub(crate) fn clip(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        format!("{}...", value.chars().take(max_chars).collect::<String>())
    }
}

