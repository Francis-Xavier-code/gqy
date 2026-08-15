//! Homebrew (brew) 工具组:Homebrew 官方包搜索、详情与 Homebrew 服务状态。
//!
//! 数据源为 formulae.brew.sh 官方 JSON API;搜索优先走本地 `brew search`
//! (结果含第三方 tap),brew 未安装时回退到官方 API 全量列表过滤。

use super::{http_response, ToolRegistry, ToolSpec};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command;

const FORMULAE_API_BASE: &str = "https://formulae.brew.sh/api";
const GITHUB_STATUS_URL: &str = "https://www.githubstatus.com/api/v2/status.json";
const SEARCH_TIMEOUT_SECONDS: u64 = 30;

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new(
        "brew_search_packages",
        "Search Homebrew formulae and casks. Uses the local `brew search` when available, otherwise the official formulae.brew.sh API.",
        json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer","description":"Maximum results. Defaults to 10, capped at 50."},"include_casks":{"type":"boolean","description":"Include Homebrew Casks (GUI apps). Defaults to true."}},"required":["query"],"additionalProperties":false}),
        |args| async move { brew_search(args).await },
    ));
    registry.register(ToolSpec::new(
        "brew_get_package_info",
        "Get Homebrew formula or cask details via the official formulae.brew.sh API.",
        json!({"type":"object","properties":{"package_name":{"type":"string"},"kind":{"type":"string","enum":["auto","formula","cask"],"description":"auto tries formula first, then cask."}},"required":["package_name"],"additionalProperties":false}),
        |args| async move { brew_info(args).await },
    ));
    registry.register(ToolSpec::new(
        "brew_check_status",
        "Check Homebrew service health: formulae.brew.sh API reachability and GitHub status (Homebrew bottles and the API are served from GitHub infrastructure).",
        super::empty_parameters(),
        |_| async move { brew_status().await },
    ));
}

async fn brew_search(args: Value) -> Result<String> {
    let query = required(&args, "query")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .min(50) as usize;
    let include_casks = args
        .get("include_casks")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let results = if let Some(local) = local_brew_search(&query, include_casks).await {
        local
    } else {
        api_brew_search(&query, limit, include_casks).await?
    };

    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "query": query,
        "results": results.into_iter().take(limit).collect::<Vec<_>>(),
    }))?)
}

/// `brew search` 输出形如 `==> Formulae\nfoo bar\n==> Casks\nbaz`,按节解析。
async fn local_brew_search(query: &str, include_casks: bool) -> Option<Vec<Value>> {
    let output = Command::new("brew")
        .arg("search")
        .arg(query)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();
    let mut section = "formula";
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("==> ") {
            section = if line.contains("Cask") {
                "cask"
            } else {
                "formula"
            };
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if section == "cask" && !include_casks {
            continue;
        }
        for name in line.split_whitespace() {
            results.push(json!({
                "name": name,
                "kind": section,
                "url": format!("https://formulae.brew.sh/{}/{}", if section == "cask" { "cask" } else { "formula" }, name),
            }));
        }
    }
    if results.is_empty() {
        None
    } else {
        Some(results)
    }
}

/// 官方 API 全量列表过滤(brew 未安装时的回退路径)。
async fn api_brew_search(query: &str, limit: usize, include_casks: bool) -> Result<Vec<Value>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(SEARCH_TIMEOUT_SECONDS))
        .user_agent("gqy-brew-search/0.1")
        .build()?;
    let lower = query.to_ascii_lowercase();
    let mut results = Vec::new();

    let formula_url = format!("{FORMULAE_API_BASE}/formula.json");
    let resp = client.get(&formula_url).send().await?.error_for_status()?;
    let data: Value = http_response::read_json(resp, 32 * 1024 * 1024).await?;
    if let Some(list) = data.as_array() {
        for item in list {
            if results.len() >= limit {
                break;
            }
            let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
            let desc = item.get("desc").and_then(Value::as_str).unwrap_or_default();
            if name.to_ascii_lowercase().contains(&lower)
                || desc.to_ascii_lowercase().contains(&lower)
            {
                results.push(json!({
                    "name": name,
                    "kind": "formula",
                    "description": desc,
                    "url": format!("https://formulae.brew.sh/formula/{name}"),
                }));
            }
        }
    }

    if include_casks && results.len() < limit {
        let cask_url = format!("{FORMULAE_API_BASE}/cask.json");
        if let Ok(resp) = client.get(&cask_url).send().await {
            if let Ok(resp) = resp.error_for_status() {
                let data: Value = resp.json().await.unwrap_or(Value::Null);
                if let Some(list) = data.as_array() {
                    for item in list {
                        if results.len() >= limit {
                            break;
                        }
                        let name = item
                            .get("token")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let desc = item.get("desc").and_then(Value::as_str).unwrap_or_default();
                        if name.to_ascii_lowercase().contains(&lower)
                            || desc.to_ascii_lowercase().contains(&lower)
                        {
                            results.push(json!({
                                "name": name,
                                "kind": "cask",
                                "description": desc,
                                "url": format!("https://formulae.brew.sh/cask/{name}"),
                            }));
                        }
                    }
                }
            }
        }
    }
    Ok(results)
}

async fn brew_info(args: Value) -> Result<String> {
    let package = required(&args, "package_name")?;
    let kind = args
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("auto")
        .trim();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("gqy-brew-info/0.1")
        .build()?;

    let mut attempts: Vec<(&str, &str)> = Vec::new();
    match kind {
        "formula" => attempts.push(("formula", &package)),
        "cask" => attempts.push(("cask", &package)),
        _ => {
            attempts.push(("formula", &package));
            attempts.push(("cask", &package));
        }
    }

    for (kind, name) in attempts {
        let url = format!(
            "{FORMULAE_API_BASE}/{kind}/{}.json",
            urlencoding::encode(name)
        );
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            continue;
        }
        let data: Value = resp.json().await?;
        return Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "kind": kind,
            "package_name": package,
            "data": normalize_info_item(kind, &data),
        }))?);
    }
    bail!("Homebrew package not found (formula or cask): {package}")
}

fn normalize_info_item(kind: &str, item: &Value) -> Value {
    let name = item
        .get("name")
        .or_else(|| item.get("token"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut normalized = json!({
        "name": name,
        "kind": kind,
        "desc": item.get("desc"),
        "homepage": item.get("homepage"),
        "license": item.get("license"),
        "tap": item.get("tap"),
        "formula_url": if name.is_empty() { Value::Null } else { json!(format!("https://formulae.brew.sh/{kind}/{name}")) },
    });
    if kind == "cask" {
        normalized["version"] = item.get("version");
        normalized["deprecated"] = item.get("deprecated");
        normalized["disabled"] = item.get("disabled");
        normalized["artifacts"] = item.get("artifacts");
        normalized["dependencies"] = item.get("depends_on");
        normalized["url"] = item
            .pointer("/url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .map(Value::String)
            .unwrap_or(Value::Null);
    } else {
        normalized["versions"] = item.get("versions");
        normalized["deprecated"] = item.get("deprecated");
        normalized["disabled"] = item.get("disabled");
        normalized["dependencies"] = item.get("dependencies");
        normalized["build_dependencies"] = item.get("build_dependencies");
        normalized["bottle"] = item.get("bottle");
        normalized["installed"] = item.get("installed");
        normalized["url"] = item
            .pointer("/urls/stable/url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .map(Value::String)
            .unwrap_or(Value::Null);
    }
    normalized
}

async fn brew_status() -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("gqy-brew-status/0.1")
        .build()?;

    let api_resp = client
        .get(format!("{FORMULAE_API_BASE}/formula.json"))
        .send()
        .await;
    let api_up = api_resp
        .as_ref()
        .map(|resp| resp.status().is_success())
        .unwrap_or(false);

    let github: Value = client
        .get(GITHUB_STATUS_URL)
        .send()
        .await
        .map(|resp| resp.json::<Value>())
        .ok()
        .and_then(|future| future.ok())
        .unwrap_or(Value::Null);
    let github_indicator = github
        .pointer("/status/indicator")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let github_description = github
        .pointer("/status/description")
        .and_then(Value::as_str)
        .unwrap_or("unknown");

    let current_state = if !api_up || github_indicator == "major" {
        "down"
    } else if github_indicator == "minor" {
        "degraded"
    } else {
        "up"
    };
    let is_degraded = current_state != "up";
    let degraded_reason = if is_degraded {
        Some(format!(
            "formulae.brew.sh API up: {api_up}; GitHub status: {github_description}"
        ))
    } else {
        None
    };

    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "current_state": current_state,
        "is_degraded": is_degraded,
        "degraded_reason": degraded_reason,
        "components": {
            "formulae_api": if api_up { "up" } else { "down" },
            "github_status_indicator": github_indicator,
            "github_status_description": github_description,
        },
        "source": GITHUB_STATUS_URL,
    }))?)
}

fn required(args: &Value, key: &str) -> Result<String> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        bail!("{key} is required")
    } else {
        Ok(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_formula_and_cask_items() {
        let formula = json!({
            "name": "wget",
            "desc": "Internet file retriever",
            "homepage": "https://www.gnu.org/software/wget/",
            "license": "GPL-3.0-or-later",
            "versions": {"stable": "1.25.0"},
            "dependencies": ["openssl@3"],
            "deprecated": false,
        });
        let normalized = normalize_info_item("formula", &formula);
        assert_eq!(normalized["name"], "wget");
        assert_eq!(normalized["kind"], "formula");
        assert_eq!(normalized["versions"]["stable"], "1.25.0");
        assert_eq!(
            normalized["formula_url"],
            "https://formulae.brew.sh/formula/wget"
        );

        let cask = json!({
            "token": "visual-studio-code",
            "version": "1.90.0",
            "desc": "VS Code",
            "artifacts": [],
        });
        let normalized = normalize_info_item("cask", &cask);
        assert_eq!(normalized["name"], "visual-studio-code");
        assert_eq!(normalized["version"], "1.90.0");
    }

    #[test]
    fn required_rejects_empty_values() {
        assert!(required(&json!({"query": "  "}), "query").is_err());
        assert!(required(&json!({"query": "ripgrep"}), "query").is_ok());
    }
}
