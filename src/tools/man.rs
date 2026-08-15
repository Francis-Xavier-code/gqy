//! 手册查询:macOS 上优先用本地 `man`(权威且离线可用),回退到
//! manpagez.com 的 Apple man pages 在线副本。

use super::{html_conversion, http_response, ToolRegistry, ToolSpec};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command;

const MANPAGEZ_BASE: &str = "https://www.manpagez.com";
const MAN_SECTIONS: [&str; 8] = ["1", "5", "8", "7", "2", "3", "4", "6"];

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new(
        "online_man_search",
        "Search man pages with the local `man -k` (macOS ships its own man pages).",
        json!({"type":"object","properties":{"query":{"type":"string"},"section":{"type":"string"},"limit":{"type":"integer"}},"required":["query"],"additionalProperties":false}),
        |args| async move { search(args).await },
    ));
    registry.register(ToolSpec::new(
        "online_man_get_page",
        "Fetch a man page from the local `man` command, falling back to manpagez.com (Apple man pages).",
        json!({"type":"object","properties":{"name":{"type":"string"},"section":{"type":"string"},"source":{"type":"string","enum":["auto","local","manpagez"]},"max_chars":{"type":"integer","description":"Maximum returned characters. Use at least 8000 for normal reading; omit unless user asks for a short excerpt."}},"required":["name"],"additionalProperties":false}),
        |args| async move { get_page(args).await },
    ));
}

async fn search(args: Value) -> Result<String> {
    let query = required(&args, "query")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .min(50) as usize;
    let output = Command::new("man")
        .arg("-k")
        .arg(&query)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        bail!(
            "local man -k failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let results = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(limit)
        .collect::<Vec<_>>();
    if results.is_empty() {
        Ok(format!("No man page search results for {query}"))
    } else {
        Ok(results.join("
"))
    }
}

async fn get_page(args: Value) -> Result<String> {
    let name = required(&args, "name")?;
    let section = args
        .get("section")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let source = args.get("source").and_then(Value::as_str).unwrap_or("auto");
    let max_chars = args
        .get("max_chars")
        .and_then(Value::as_u64)
        .unwrap_or(16_000)
        .clamp(2_000, 100_000) as usize;

    let try_local = source == "auto" || source == "local";
    if try_local {
        if let Some(text) = local_man_page(&name, section).await {
            return Ok(clip(&format!("Source: local man(1)\n\n{text}"), max_chars));
        }
    }
    let try_manpagez = source == "auto" || source == "manpagez";
    if try_manpagez {
        let sections: Vec<&str> = if section.is_empty() {
            MAN_SECTIONS.to_vec()
        } else {
            vec![section]
        };
        for sec in sections {
            let url = format!("{MANPAGEZ_BASE}/man/{sec}/{name}/");
            if let Ok(html) = fetch_text(&url).await {
                let text = html_conversion::to_text_async(html, 120).await?;
                return Ok(clip(&format!("Source: {url}\n\n{text}"), max_chars));
            }
        }
    }
    bail!("man page not found: {name}")
}

async fn local_man_page(name: &str, section: &str) -> Option<String> {
    let mut command = Command::new("man");
    command.env("MANPAGER", "cat").env("PAGER", "cat");
    if !section.is_empty() {
        command.arg(section);
    }
    command.arg(name);
    command.stdin(Stdio::null());
    command.kill_on_drop(true);
    let output = command.output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

async fn fetch_text(url: &str) -> Result<String> {
    let response = reqwest::get(url).await?.error_for_status()?;
    http_response::read_text(response, http_response::MAX_HTML_RESPONSE_BYTES).await
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

fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!(
            "{}\n...[truncated]",
            text.chars().take(max_chars).collect::<String>()
        )
    }
}
