//! score — 自 src/tools/memes.rs 拆分。

use super::*;

pub(crate) fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
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

pub(crate) fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn normalize(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_punctuation()
                || matches!(
                    ch,
                    '，' | '。' | '！' | '？' | '、' | '；' | '：' | '（' | '）' | '“' | '”'
                )
            {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>()
}

fn search_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for token in query.split_whitespace() {
        if token.chars().count() > 1 {
            terms.push(token.to_string());
        }
        if token.chars().any(|ch| !ch.is_ascii()) {
            let chars = token.chars().collect::<Vec<_>>();
            for pair in chars.windows(2) {
                terms.push(pair.iter().collect());
            }
        }
    }
    terms.sort();
    terms.dedup();
    terms
}

pub(crate) fn score_meme(item: &MemeItem, query: &str, tags: &[String]) -> f32 {
    let query = normalize(&format!("{query} {}", tags.join(" ")));
    let terms = search_terms(&query);
    if terms.is_empty() {
        return 0.1;
    }
    let name = normalize(&format!("{} {}", item.name.zh, item.name.en));
    let description = normalize(&item.description);
    let usage = normalize(&item.usage);
    let avoid = normalize(&item.avoid);
    let tag_text = normalize(&item.tags.join(" "));
    let mut score: f32 = 0.0;
    for term in terms {
        if tag_text.contains(&term) {
            score += 3.0;
        }
        if name.contains(&term) {
            score += 2.5;
        }
        if usage.contains(&term) {
            score += 2.0;
        }
        if description.contains(&term) {
            score += 1.2;
        }
        if !avoid.is_empty() && avoid.contains(&term) {
            score -= 2.5;
        }
    }
    let haystack = format!("{name} {description} {usage} {tag_text}");
    if !query.is_empty() && haystack.contains(&query) {
        score += 2.0;
    }
    score.max(0.0)
}
