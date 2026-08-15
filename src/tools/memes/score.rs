//! score — 自 src/tools/memes.rs 拆分。

use super::*;

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        bail!("{key} is required")
    }
    Ok(value)
}

