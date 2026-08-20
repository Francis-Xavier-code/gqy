//! 资源安全审查与安装状态管理：统一为「脚本 / Skill 注册」提供
//! review→确认→安装 的 ledger 记录，供 AI 审查工具与 CLI 共用。
//!
//! 审查记录写在 `state/resource-review-state.json`（已登记进
//! `transfer/registry.rs`，随 `gqy export` 携带），安装记录写在
//! `state/resource-install-ledger.json`。

use crate::paths::GQYPaths;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RESOURCE_REVIEW_STATE_FILE: &str = "resource-review-state.json";
const RESOURCE_INSTALL_LEDGER_FILE: &str = "resource-install-ledger.json";
const REVIEW_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// 登记一次审查结论。`verdict` 为 `allow`（低风险，建议安装）/
/// `caution`（中风险，谨慎安装）/ `block`（高风险，禁止安装）。
pub(crate) fn record_review(
    paths: &GQYPaths,
    kind: &str,
    name: &str,
    source: &str,
    sha256: &str,
    verdict: &str,
    reason: &str,
) -> Result<()> {
    fs::create_dir_all(&paths.state_dir)?;
    let mut state = load_review_state(paths)?;
    state[format!("{kind}:{name}")] = json!({
        "kind": kind,
        "name": name,
        "source": source,
        "sha256": sha256,
        "verdict": verdict,
        "reason": reason,
        "reviewed_at_unix": current_unix_seconds(),
        "install_confirmed": false,
        "installed": false,
    });
    write_review_state(paths, &state)?;
    Ok(())
}

/// 用户在看过审查后明确确认安装。返回是否允许安装：
/// 仅当存在有效（未过期）审查、内容未变化且 verdict 不是 `block` 时放行。
pub(crate) fn confirm_install(
    paths: &GQYPaths,
    kind: &str,
    name: &str,
    expected_sha256: &str,
) -> Result<bool> {
    let key = format!("{kind}:{name}");
    let mut state = load_review_state(paths)?;
    let Some(entry) = state.get_mut(&key) else {
        bail!("{kind} `{name}` 尚未审查，请先审查再安装");
    };
    if entry["verdict"].as_str() == Some("block") {
        return Ok(false);
    }
    let recorded_sha = entry["sha256"].as_str().unwrap_or_default();
    if !recorded_sha.eq_ignore_ascii_case(expected_sha256) {
        bail!("{kind} `{name}` 内容已变化（sha256 不匹配），需要重新审查");
    }
    if is_review_expired(entry) {
        bail!("{kind} `{name}` 的审查已过期，请重新审查");
    }
    entry["install_confirmed"] = json!(true);
    entry["confirmed_at_unix"] = json!(current_unix_seconds());
    write_review_state(paths, &state)?;
    Ok(true)
}

/// 安装完成后记录到 ledger，供 `gqy resources status` 追溯。
pub(crate) fn record_install(
    paths: &GQYPaths,
    kind: &str,
    name: &str,
    source: &str,
    sha256: &str,
) -> Result<()> {
    fs::create_dir_all(&paths.state_dir)?;
    let mut ledger = load_install_ledger(paths)?;
    ledger[format!("{kind}:{name}")] = json!({
        "kind": kind,
        "name": name,
        "source": source,
        "sha256": sha256,
        "installed_at_unix": current_unix_seconds(),
    });
    fs::write(
        paths.state_dir.join(RESOURCE_INSTALL_LEDGER_FILE),
        format!("{}\n", serde_json::to_string_pretty(&ledger)?),
    )?;
    Ok(())
}

/// 汇总状态：返回每类资源的审查/安装情况，供 CLI `resources status` 输出。
pub(crate) fn status_summary(paths: &GQYPaths) -> Result<Value> {
    let state = load_review_state(paths)?;
    let ledger = load_install_ledger(paths)?;
    let mut skills = Vec::new();
    let mut scripts = Vec::new();
    if let Some(object) = state.as_object() {
        for (key, entry) in object {
            if !entry.is_object() {
                continue;
            }
            let mut item = entry.clone();
            item["installed"] = json!(ledger.get(key).is_some());
            if key.starts_with("skill:") {
                skills.push(item);
            } else if key.starts_with("script:") {
                scripts.push(item);
            }
        }
    }
    skills.sort_by_key(|item| item["name"].as_str().unwrap_or_default().to_string());
    scripts.sort_by_key(|item| item["name"].as_str().unwrap_or_default().to_string());
    Ok(json!({
        "ok": true,
        "skills": skills,
        "scripts": scripts,
        "hint": "通过自然语言或 CLI（gqy resources）触发 AI 审查后再安装；block 类资源禁止安装。",
    }))
}

/// 清理过期的审查与安装记录，返回移除的审查条目数。
pub(crate) fn prune_review_state(paths: &GQYPaths) -> Result<usize> {
    let path = paths.state_dir.join(RESOURCE_REVIEW_STATE_FILE);
    if !path.exists() {
        return Ok(0);
    }
    let mut state = load_review_state(paths)?;
    let before = state.as_object().map(|o| o.len()).unwrap_or(0);
    if let Some(object) = state.as_object_mut() {
        let expired = object
            .iter()
            .filter(|(_, entry)| entry.is_object() && is_review_expired(entry))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in expired {
            object.remove(&key);
        }
    }
    let after = state.as_object().map(|o| o.len()).unwrap_or(0);
    write_review_state(paths, &state)?;
    Ok(before.saturating_sub(after))
}

fn load_review_state(paths: &GQYPaths) -> Result<Value> {
    let path = paths.state_dir.join(RESOURCE_REVIEW_STATE_FILE);
    if !path.exists() {
        return Ok(json!({}));
    }
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

fn write_review_state(paths: &GQYPaths, state: &Value) -> Result<()> {
    fs::write(
        paths.state_dir.join(RESOURCE_REVIEW_STATE_FILE),
        format!("{}\n", serde_json::to_string_pretty(state)?),
    )?;
    Ok(())
}

fn load_install_ledger(paths: &GQYPaths) -> Result<Value> {
    let path = paths.state_dir.join(RESOURCE_INSTALL_LEDGER_FILE);
    if !path.exists() {
        return Ok(json!({}));
    }
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

fn is_review_expired(entry: &Value) -> bool {
    let reviewed = entry["reviewed_at_unix"].as_u64().unwrap_or(0);
    let age = current_unix_seconds().saturating_sub(reviewed);
    age > REVIEW_TTL.as_secs()
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// 计算文件/目录的 sha256（目录按相对路径排序后逐文件哈希）。
pub(crate) fn sha256_of_resource(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hash_path(&mut hasher, path)
        .with_context(|| format!("computing sha256 of {}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_path(hasher: &mut Sha256, path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        bail!("资源不能包含符号链接: {}", path.display());
    }
    if metadata.is_file() {
        let mut file = File::open(path)?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    } else if metadata.is_dir() {
        let mut children = fs::read_dir(path)?.collect::<std::result::Result<Vec<_>, _>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for entry in children {
            hash_path(hasher, &entry.path())?;
        }
    }
    Ok(())
}
