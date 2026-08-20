//! 出网请求录制(验收:要能亲眼确认发给模型的完整请求体,例如 dev
//! 请求有没有被注入不必要的东西)。
//!
//! 抓在三个序列化后的出网口(chat/anthropic/responses),记录的就是
//! `.json(request)` 将要发出的同一结构——所有 extra_body、思考档、消息
//! 变换都已生效。默认关闭;`gqy daemon logs request` 在监控期间开启,
//! 环境变量 `GQY_LOG_REQUESTS=1` 可常开(直连 REPL 也生效)。
//! 完整体积大(长上下文可达数百 KB/请求),全量落 JSONL 文件,不进
//! 常规日志。开关是进程级内存位,daemon 重启自动回到关闭。
//!
//! 落盘前会对 body 中的敏感字段(api_key / Authorization / x-api-key 等)
//! 做掩码脱敏,避免密钥以明文形式写入 requests-*.jsonl。

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use serde_json::Value;

static ENABLED: AtomicBool = AtomicBool::new(false);
static ENV_INIT: OnceLock<()> = OnceLock::new();
static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 客户端构造时安装日志目录(幂等)。
pub fn install_dir(dir: PathBuf) {
    let _ = LOG_DIR.set(dir);
}

fn env_init() {
    ENV_INIT.get_or_init(|| {
        if std::env::var_os("GQY_LOG_REQUESTS").is_some_and(|value| value != "0") {
            ENABLED.store(true, Ordering::Relaxed);
        }
    });
}

pub fn set_enabled(enabled: bool) {
    env_init();
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    env_init();
    ENABLED.load(Ordering::Relaxed)
}

/// 当天的录制文件路径(目录未安装时为 None)。
pub fn current_file() -> Option<PathBuf> {
    let date = chrono::Utc::now().format("%Y-%m-%d");
    Some(LOG_DIR.get()?.join(format!("requests-{date}.jsonl")))
}

/// 追加一条完整请求记录。录制关闭时零开销;写盘失败只警告,绝不影响
/// 请求本身。落盘前对 body 中的敏感字段做掩码脱敏。
pub fn record<T: serde::Serialize>(
    provider: &str,
    model: &str,
    kind: &str,
    scope: &str,
    url: &str,
    request: &T,
) {
    if !enabled() {
        return;
    }
    let Some(path) = current_file() else {
        return;
    };
    let body = match serde_json::to_value(request) {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(%error, "request log: serialize failed");
            return;
        }
    };
    let body = redact_sensitive_fields(body);
    let line = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "provider": provider,
        "model": model,
        "kind": kind,
        "scope": scope,
        "url": url,
        "body": body,
    });
    let write = || -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let mut serialized = line.to_string();
        serialized.push('\n');
        file.write_all(serialized.as_bytes())
    };
    if let Err(error) = write() {
        tracing::warn!(%error, path = %path.display(), "request log: append failed");
    }
}

/// 字段名（小写）命中即视为敏感，值会被掩码。覆盖 OpenAI 兼容的
/// `api_key`、HTTP 头风格字段（`authorization` / `x-api-key`），以及常见
/// 网关/代理注入进 `extra_body` 的密钥键。
const SENSITIVE_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "authorization",
    "x-api-key",
    "token",
    "access_token",
    "refresh_token",
    "secret",
    "secret_key",
    "client_secret",
    "password",
];

/// 掩码一个敏感值：`sk-` 前缀保留前 7 个字符，其余保留前 4 个字符，
/// 之后替换为 `***`。按字符（而非字节）取前缀，避免在 UTF-8 多字节
/// 字符中间切片；短值会连同 `***` 一起落盘，以明确标记已被脱敏。
fn mask_secret(value: &str) -> String {
    const MASK: &str = "***";
    let keep = if value.starts_with("sk-") { 7 } else { 4 };
    let prefix: String = value.chars().take(keep).collect();
    format!("{prefix}{MASK}")
}

/// 递归遍历请求 body，将敏感字段的值替换为掩码。
fn redact_sensitive_fields(value: Value) -> Value {
    match value {
        Value::Object(mut object) => {
            for (key, child) in object.iter_mut() {
                let sensitive = SENSITIVE_KEYS
                    .iter()
                    .any(|candidate| key.eq_ignore_ascii_case(candidate));
                if sensitive {
                    let masked = match child {
                        Value::String(value) => mask_secret(value),
                        _ => "***".to_string(),
                    };
                    *child = Value::String(masked);
                } else {
                    let nested = child.take();
                    *child = redact_sensitive_fields(nested);
                }
            }
            Value::Object(object)
        }
        Value::Array(items) => {
            Value::Array(items.into_iter().map(redact_sensitive_fields).collect())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_nested_api_key_fields() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "api_key": "sk-abcdef123456",
            "extra_body": {
                "authorization": "Bearer sk-abcdef123456",
                "x-api-key": "very-secret-value"
            },
            "messages": [{"role": "user", "content": "hi"}]
        });
        let redacted = redact_sensitive_fields(body);
        assert_eq!(redacted["api_key"], "sk-abcd***");
        assert_eq!(redacted["extra_body"]["authorization"], "Bear***");
        assert_eq!(redacted["extra_body"]["x-api-key"], "very***");
        assert_eq!(redacted["model"], "gpt-4o");
        assert_eq!(redacted["messages"][0]["content"], "hi");
    }

    #[test]
    fn short_and_non_string_secrets_are_masked() {
        let body = serde_json::json!({
            "password": "",
            "token": 42,
            "secret_key": "ab"
        });
        let redacted = redact_sensitive_fields(body);
        assert_eq!(redacted["password"], "***");
        assert_eq!(redacted["token"], "***");
        assert_eq!(redacted["secret_key"], "ab***");
    }

    #[test]
    fn non_sensitive_values_pass_through_unchanged() {
        let body = serde_json::json!(["a", {"content": "keep"}, 7, true]);
        assert_eq!(redact_sensitive_fields(body.clone()), body);
    }
}
