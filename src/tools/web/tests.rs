//! tests — 自 src/tools/web.rs 外移。
#![cfg(test)]

pub(crate) use super::*;

#[test]
fn clips_fetch_output_with_notice() {
    let output = clip_fetch_output("abcdef", 3);

    assert_eq!(output, "abc\n\n[content truncated from 6 chars to 3 chars]");
}

#[test]
fn keeps_short_fetch_output_unchanged() {
    assert_eq!(clip_fetch_output("abc", 3), "abc");
}

#[test]
fn parses_exa_public_text_blocks() {
    let text = "Title: 第一条结果\nURL: https://example.com/a\nPublished: 2025-09-28T00:00:00.000Z\nAuthor: torvalds\nHighlights:\n第一段\n第二段\n\n---\n\nTitle: 第二条\nURL: https://example.com/b\nAuthor: N/A\nHighlights:\n内容\n";
    let results = exa_public_results(text, 10);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["title"], "第一条结果");
    assert_eq!(results[0]["url"], "https://example.com/a");
    assert!(results[0]["snippet"]
        .as_str()
        .unwrap()
        .contains("Published: 2025-09-28"));
    assert!(results[0]["snippet"].as_str().unwrap().contains("torvalds"));
    assert!(results[0]["raw_content"]
        .as_str()
        .unwrap()
        .contains("第二段"));
    // N/A 作者不进 snippet
    assert!(!results[1]["snippet"].as_str().unwrap().contains("N/A"));

    let formatted = format_search_results("测试", "Exa (free quota)", results).unwrap();
    assert!(formatted.contains("### 1. 第一条结果"));
    assert!(formatted.contains("**URL**: https://example.com/b"));
}

#[test]
fn exa_joins_auto_order_without_key() {
    let config = WebPluginConfig::default();
    let order = search_provider_order("auto", &config).unwrap();
    let ids = order.iter().map(|p| p.id()).collect::<Vec<_>>();
    // 无任何配置时：免 key Exa 优先，爬虫兜底
    assert_eq!(ids, vec!["exa", "duckduckgo"]);
}

#[test]
fn exa_with_key_is_a_primary_provider() {
    let config = WebPluginConfig {
        exa_api_keys: vec!["k".to_string()],
        ..WebPluginConfig::default()
    };
    let providers = configured_primary_providers(&config);
    assert!(providers.iter().any(|p| matches!(p, SearchProvider::Exa)));
    assert!(search_provider_order("exa", &config).is_ok());
}

/// 真实网络实测：cargo test --bin gqy -- --ignored exa_free_quota
#[tokio::test]
#[ignore = "hits the real Exa MCP endpoint"]
async fn exa_free_quota_live_search() {
    let client = reqwest::Client::builder()
        .timeout(CRAWLER_TIMEOUT)
        .build()
        .unwrap();
    let output = search_exa_public(&client, "macOS kernel release", 2)
        .await
        .unwrap();
    assert!(output.contains("**Provider**: Exa (free quota)"));
    assert!(output.contains("**URL**:"));
}
