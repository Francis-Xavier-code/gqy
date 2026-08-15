//! Deterministic Tencent QQ avatar URLs.
//!
//! QQ avatars are served from fixed CDN endpoints keyed only by the numeric
//! QQ ID / group ID, so no OneBot API round-trip is needed. URLs built here
//! are trusted by the scoped `vision_analyze` gate: the host is fixed and the
//! only variable parts are digits, which leaves no injection or exfiltration
//! surface.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Default avatar size requested from the CDN (largest stable variant).
pub(crate) const DEFAULT_AVATAR_SIZE: u32 = 640;

/// Avatars are small; anything past this is not an avatar response.
const MAX_AVATAR_BYTES: usize = 8 * 1024 * 1024;

const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Download a trusted avatar URL into `dir` as `{file_stem}.{ext}` and
/// return the saved path. Only URLs accepted by [`is_trusted_avatar_url`]
/// are fetched; the deterministic filename makes repeat downloads overwrite
/// instead of accumulating.
pub(crate) async fn download_avatar(url: &str, dir: &Path, file_stem: &str) -> Result<PathBuf> {
    if !is_trusted_avatar_url(url) {
        bail!("refusing to download a non-avatar URL");
    }
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(4))
        .build()
        .context("failed to build the avatar download client")?;
    let response = client
        .get(url)
        .send()
        .await
        .context("头像下载请求失败")?;
    if !response.status().is_success() {
        bail!("头像下载失败：HTTP {}", response.status());
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_AVATAR_BYTES as u64)
    {
        bail!("头像文件超过 8 MiB 上限");
    }
    let extension = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .map_or("jpg", |mime| match mime {
            "image/png" => "png",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "jpg",
        });
    let bytes = response.bytes().await.context("读取头像数据失败")?;
    if bytes.is_empty() {
        bail!("头像响应为空");
    }
    if bytes.len() > MAX_AVATAR_BYTES {
        bail!("头像文件超过 8 MiB 上限");
    }
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("failed to create avatar dir {}", dir.display()))?;
    let path = dir.join(format!("{file_stem}.{extension}"));
    tokio::fs::write(&path, &bytes)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// Avatar URL for a QQ user. Returns `None` unless `user_id` is purely
/// numeric (QQ IDs always are; anything else would build a URL we refuse to
/// trust downstream).
pub(crate) fn user_avatar_url(user_id: &str, size: u32) -> Option<String> {
    if !is_numeric_id(user_id) {
        return None;
    }
    Some(format!(
        "https://q.qlogo.cn/headimg_dl?dst_uin={user_id}&spec={size}"
    ))
}

/// Avatar URL for a QQ group.
pub(crate) fn group_avatar_url(group_id: &str, size: u32) -> Option<String> {
    if !is_numeric_id(group_id) {
        return None;
    }
    Some(format!("https://p.qlogo.cn/gh/{group_id}/{group_id}/{size}"))
}

/// Whether `url` is exactly one of the avatar URL shapes produced above.
/// Used by the scoped vision gate to admit avatar lookups while still
/// rejecting every other remote URL.
pub(crate) fn is_trusted_avatar_url(url: &str) -> bool {
    is_user_avatar_url(url) || is_group_avatar_url(url)
}

fn is_user_avatar_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://q.qlogo.cn/headimg_dl?dst_uin=") else {
        return false;
    };
    let Some((uin, spec)) = rest.split_once("&spec=") else {
        return false;
    };
    is_numeric_id(uin) && is_numeric_id(spec)
}

fn is_group_avatar_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://p.qlogo.cn/gh/") else {
        return false;
    };
    let mut parts = rest.split('/');
    let (Some(first), Some(second), Some(size), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    first == second && is_numeric_id(first) && is_numeric_id(size)
}

fn is_numeric_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_user_avatar_urls_for_numeric_ids_only() {
        assert_eq!(
            user_avatar_url("123456", DEFAULT_AVATAR_SIZE).as_deref(),
            Some("https://q.qlogo.cn/headimg_dl?dst_uin=123456&spec=640")
        );
        assert_eq!(user_avatar_url("abc", 640), None);
        assert_eq!(user_avatar_url("", 640), None);
        assert_eq!(user_avatar_url("123 456", 640), None);
    }

    #[test]
    fn builds_group_avatar_urls_for_numeric_ids_only() {
        assert_eq!(
            group_avatar_url("987654321", 640).as_deref(),
            Some("https://p.qlogo.cn/gh/987654321/987654321/640")
        );
        assert_eq!(group_avatar_url("g123", 640), None);
    }

    #[test]
    fn trusts_exactly_the_urls_we_build() {
        for (id, size) in [("10001", 100), ("123456789", 640)] {
            assert!(is_trusted_avatar_url(&user_avatar_url(id, size).unwrap()));
            assert!(is_trusted_avatar_url(&group_avatar_url(id, size).unwrap()));
        }
    }

    #[test]
    fn rejects_lookalike_urls() {
        for url in [
            "http://q.qlogo.cn/headimg_dl?dst_uin=123&spec=640",
            "https://q.qlogo.cn.evil.com/headimg_dl?dst_uin=123&spec=640",
            "https://q.qlogo.cn/headimg_dl?dst_uin=123&spec=640&extra=x",
            "https://q.qlogo.cn/headimg_dl?dst_uin=123x&spec=640",
            "https://q.qlogo.cn/headimg_dl?dst_uin=&spec=640",
            "https://p.qlogo.cn/gh/123/456/640",
            "https://p.qlogo.cn/gh/123/123/640/extra",
            "https://p.qlogo.cn/gh/123/123/../../secret",
            "https://p.qlogo.cn/gh/123/123",
            "https://example.com/avatar.png",
        ] {
            assert!(!is_trusted_avatar_url(url), "should reject: {url}");
        }
    }
}
