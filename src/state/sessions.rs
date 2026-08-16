//! sessions — 自 src/state/mod.rs 拆分。

// ponytail: 与 src/web/sessions*.rs（web API 会话处理）易混淆：本文件是会话 DB/状态；
// 改名（如 session_store.rs）后删除本注释。
pub(crate) use super::*;

pub(crate) fn artifact_media_type(path: &Path) -> (&'static str, &'static str) {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => ("text/markdown; charset=utf-8", "markdown"),
        "html" | "htm" => ("text/html; charset=utf-8", "html"),
        "pdf" => ("application/pdf", "pdf"),
        "json" | "jsonl" => ("application/json; charset=utf-8", "json"),
        "txt" | "log" | "csv" | "tsv" => ("text/plain; charset=utf-8", "text"),
        "css" => ("text/css; charset=utf-8", "code"),
        "js" | "mjs" | "cjs" => ("text/javascript; charset=utf-8", "code"),
        "xml" => ("application/xml; charset=utf-8", "code"),
        "rs" | "jsx" | "ts" | "tsx" | "py" | "go" | "java" | "c" | "cc" | "cpp" | "h" | "hpp"
        | "cs" | "rb" | "php" | "swift" | "kt" | "kts" | "sh" | "bash" | "zsh" | "fish"
        | "toml" | "yaml" | "yml" | "scss" | "sql" => ("text/plain; charset=utf-8", "code"),
        _ => ("application/octet-stream", "file"),
    }
}

pub(crate) fn prompt_fingerprint(system_prompt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(system_prompt.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[allow(dead_code)]
pub(crate) fn turn_chars(turn: &Turn) -> usize {
    turn.user_content.chars().count()
        + turn.assistant_content.chars().count()
        + turn
            .assistant_reasoning
            .as_deref()
            .map(str::chars)
            .map(Iterator::count)
            .unwrap_or(0)
        + turn
            .tool_reports
            .iter()
            .map(|r| r.chars().count())
            .sum::<usize>()
        + turn
            .question_exchanges
            .iter()
            .filter_map(|exchange| serde_json::to_string(exchange).ok())
            .map(|exchange| exchange.chars().count())
            .sum::<usize>()
        + turn
            .followups
            .iter()
            .map(|followup| {
                followup.content.chars().count()
                    + followup
                        .preceding_assistant_content
                        .as_deref()
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
                    + followup
                        .preceding_assistant_reasoning
                        .as_deref()
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
            })
            .sum::<usize>()
}

pub(crate) fn turns_to_entries(turns: Vec<Turn>) -> Vec<StoredConversationEntry> {
    let mut entries = Vec::with_capacity(turns.len() * 3);
    for turn in turns {
        let ts = turn.assistant_timestamp.clone().unwrap_or_default();
        entries.push(StoredConversationEntry {
            timestamp: turn.user_timestamp,
            role: "user".to_string(),
            content: turn.user_content,
            reasoning: None,
        });
        for exchange in &turn.question_exchanges {
            entries.push(StoredConversationEntry {
                timestamp: exchange.answered_at.clone(),
                role: "assistant_clarification".to_string(),
                content: crate::question::assistant_exchange_text(exchange),
                reasoning: None,
            });
            entries.push(StoredConversationEntry {
                timestamp: exchange.answered_at.clone(),
                role: "user_clarification".to_string(),
                content: crate::question::user_exchange_text(exchange),
                reasoning: None,
            });
        }
        for followup in turn.followups {
            if followup
                .preceding_assistant_content
                .as_deref()
                .is_some_and(|content| !content.trim().is_empty())
                || followup
                    .preceding_assistant_reasoning
                    .as_deref()
                    .is_some_and(|reasoning| !reasoning.trim().is_empty())
            {
                entries.push(StoredConversationEntry {
                    timestamp: followup.submitted_at.clone(),
                    role: "assistant".to_string(),
                    content: followup.preceding_assistant_content.unwrap_or_default(),
                    reasoning: followup.preceding_assistant_reasoning,
                });
            }
            entries.push(StoredConversationEntry {
                timestamp: followup.submitted_at,
                role: "user".to_string(),
                content: followup.content,
                reasoning: None,
            });
        }
        entries.push(StoredConversationEntry {
            timestamp: ts.clone(),
            role: "assistant".to_string(),
            content: turn.assistant_content,
            reasoning: turn.assistant_reasoning,
        });
        for report in turn.tool_reports {
            entries.push(StoredConversationEntry {
                timestamp: ts.clone(),
                role: "assistant".to_string(),
                content: report,
                reasoning: None,
            });
        }
    }
    entries
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StoredConversationEntry {
    pub timestamp: String,
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub reasoning: Option<String>,
}
