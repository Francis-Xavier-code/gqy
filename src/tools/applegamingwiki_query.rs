//! AppleGamingWiki 查询:macOS 游戏兼容性(原生 / Crossover / Wine / Whisky /
//! Parallels 等运行方式)。替代原 Linux 生态的 caniplayonlinux 与 ProtonDB。

use super::{html_conversion, http_response, ToolRegistry, ToolSpec};
use anyhow::{bail, Result};
use serde_json::{json, Value};

const AGW_API: &str = "https://www.applegamingwiki.com/w/api.php";
const AGW_WIKI: &str = "https://www.applegamingwiki.com/wiki";
const MAX_PAGE_CHARS: usize = 12_000;

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new(
        "query_applegamingwiki",
        "Query AppleGamingWiki for macOS game compatibility info (Native, Crossover, Wine, Whisky, Parallels, and other run methods), including ratings and notes.",
        json!({"type":"object","properties":{"query":{"type":"string","description":"Game name to search."},"title":{"type":"string","description":"Exact wiki page title; when set, reads that page directly."},"mode":{"type":"string","enum":["auto","search","page"]}},"required":[],"additionalProperties":false}),
        |args| async move { query_agw(args).await },
    ));
}

async fn query_agw(args: Value) -> Result<String> {
    let mode = args.get("mode").and_then(Value::as_str).unwrap_or("auto");
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();

    if mode == "search" || (mode == "auto" && title.is_empty()) {
        let q = if query.is_empty() { title } else { query };
        let url = format!(
            "{AGW_API}?action=opensearch&search={}&limit=8&namespace=0&format=json",
            urlencoding::encode(q)
        );
        let data: Value = reqwest::get(url).await?.error_for_status()?.json().await?;
        if mode == "search" {
            return Ok(serde_json::to_string_pretty(&json!({
                "success": true,
                "mode": "search",
                "query": q,
                "results": data,
            }))?);
        }
        if let Some(first) = data
            .get(1)
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_str)
        {
            return fetch_agw_page(first).await;
        }
        return Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "mode": "search",
            "query": q,
            "results": data,
        }))?);
    }
    fetch_agw_page(if title.is_empty() { query } else { title }).await
}

async fn fetch_agw_page(title: &str) -> Result<String> {
    if title.trim().is_empty() {
        bail!("query or title is required")
    }
    let url = format!(
        "{AGW_API}?action=parse&page={}&prop=text&format=json",
        urlencoding::encode(title)
    );
    let response = reqwest::get(url).await?.error_for_status()?;
    let data: Value =
        http_response::read_json(response, http_response::MAX_HTML_RESPONSE_BYTES).await?;
    let html = data
        .pointer("/parse/text/*")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let markdown = html_conversion::to_markdown(html.to_string()).await?;

    let compatibility = extract_compatibility(&markdown);
    let clipped = clip(&markdown, MAX_PAGE_CHARS);
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "mode": "page",
        "game": title,
        "url": format!("{AGW_WIKI}/{}", title.replace(' ', "_")),
        "compatibility": compatibility,
        "excerpt": clipped,
    }))?)
}

/// 从页面 Markdown 中提取「macOS Compatibility」小节下的运行方式表格行:
/// `| Native | Perfect | 备注 |` 这类行 → {method, rating, note}。
fn extract_compatibility(markdown: &str) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut in_compat = false;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            if trimmed.contains("macOS Compatibility") || trimmed.contains("Mac Compatibility") {
                in_compat = true;
                continue;
            }
            if in_compat {
                break;
            }
        }
        if !in_compat {
            continue;
        }
        if !trimmed.starts_with('|') {
            continue;
        }
        let cells = trimmed
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim())
            .collect::<Vec<_>>();
        if cells.len() < 2 {
            continue;
        }
        let method = cells[0];
        if method.is_empty() || method.eq_ignore_ascii_case("Method") {
            continue;
        }
        let rating = cells.get(1).copied().unwrap_or("").to_string();
        let note = cells.get(2).copied().unwrap_or("").to_string();
        rows.push(json!({
            "method": method,
            "rating": rating,
            "note": note,
        }));
    }
    rows
}

fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!(
            "{}
...[truncated]",
            text.chars().take(max_chars).collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_macos_compatibility_rows() {
        let markdown = "# Cyberpunk 2077

## macOS Compatibility

| Method | Rating | Note |
| --- | --- | --- |
| Native | Perfect | Runs great on M-series. |
| Crossover | Perfect | Runs well on medium settings. |
| Wine | Runs | Needs Whisky + GPTK. |
| Parallels | Runs | Very low FPS. |

## Availability

| Store | Available |
| --- | --- |
| Steam | Yes |";
        let rows = extract_compatibility(markdown);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0]["method"], "Native");
        assert_eq!(rows[0]["rating"], "Perfect");
        assert!(rows[0]["note"].as_str().unwrap().contains("M-series"));
        assert_eq!(rows[3]["method"], "Parallels");
    }

    #[test]
    fn skips_header_and_other_sections() {
        let markdown = "## Other

| A | B |
| --- | --- |
| x | y |

## macOS Compatibility

| Method | Rating | Note |
| --- | --- |
| Whisky | Perfect | Great. |";
        let rows = extract_compatibility(markdown);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["method"], "Whisky");
    }
}
