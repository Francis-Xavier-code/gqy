#![allow(clippy::unnecessary_sort_by)]
use anyhow::Result;
use base64::Engine;
use sha2::Digest;
use std::cell::OnceCell;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const MAX_CLIPBOARD_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const CLIPBOARD_CACHE_MAX_BYTES: u64 = 50 * 1024 * 1024;
const CLIPBOARD_CLEANUP_INTERVAL: Duration = Duration::from_secs(30);
static LAST_IMAGE_CACHE_CLEANUP: LazyLock<Mutex<HashMap<PathBuf, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct ClipboardImage {
    pub mime: String,
    pub data: Vec<u8>,
    data_url: OnceCell<String>,
}

pub enum PastedImage {
    Binary(ClipboardImage),
    Path(String),
}

impl ClipboardImage {
    pub fn new(mime: String, data: Vec<u8>) -> Self {
        Self {
            mime,
            data,
            data_url: OnceCell::new(),
        }
    }

    pub fn data_url(&self) -> &str {
        self.data_url
            .get_or_init(|| {
                let encoded = base64::engine::general_purpose::STANDARD.encode(&self.data);
                format!("data:{};base64,{}", self.mime, encoded)
            })
            .as_str()
    }

    pub fn write_temp_file(&self, cache_dir: &std::path::Path, _index: usize) -> Result<PathBuf> {
        self.write_cache_file(cache_dir, Path::new("clipboard_images"))
    }

    pub fn write_cache_file(&self, cache_dir: &Path, relative_dir: &Path) -> Result<PathBuf> {
        write_image_cache_file(cache_dir, relative_dir, &self.mime, &self.data)
    }
}

pub(crate) fn write_image_cache_file(
    cache_dir: &Path,
    relative_dir: &Path,
    mime: &str,
    data: &[u8],
) -> Result<PathBuf> {
    let dir = cache_dir.join(relative_dir);
    std::fs::create_dir_all(&dir)?;
    cleanup_image_cache_throttled(&dir);
    let ext = match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpeg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        _ => "img",
    };
    let hash = sha2::Sha256::digest(data);
    let short_hash = hex::encode(&hash[..16]);
    let path = dir.join(format!("{short_hash}.{ext}"));
    if !path.exists() {
        std::fs::write(&path, data)?;
    }
    Ok(path)
}

/// macOS 剪贴板图片:pbpaste -tiff 取 TIFF,再用系统自带 sips 转 PNG。
pub fn read_clipboard_image() -> Result<Option<ClipboardImage>> {
    if !clipboard_classes()?
        .iter()
        .any(|class| is_image_class(class))
    {
        return Ok(None);
    }
    let tiff = Command::new("pbpaste")
        .arg("-tiff")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = tiff else {
        return Ok(None);
    };
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(None);
    }
    if output.stdout.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        return Ok(None);
    }
    if let Some(png) = tiff_to_png(&output.stdout)? {
        return Ok(Some(ClipboardImage::new("image/png".to_string(), png)));
    }
    Ok(Some(ClipboardImage::new(
        "image/tiff".to_string(),
        output.stdout,
    )))
}

fn tiff_to_png(tiff: &[u8]) -> Result<Option<Vec<u8>>> {
    let dir = std::env::temp_dir();
    let id = format!("gqy-clipboard-{}", std::process::id());
    let input = dir.join(format!("{id}.tiff"));
    let output = dir.join(format!("{id}.png"));
    std::fs::write(&input, tiff)?;
    let status = Command::new("sips")
        .args(["-s", "format", "png", "-s", "formatOptions", "default"])
        .arg(&input)
        .arg("--out")
        .arg(&output)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = std::fs::remove_file(&input);
    match status {
        Ok(status) if status.success() => {
            let png = std::fs::read(&output).ok();
            let _ = std::fs::remove_file(&output);
            Ok(png.filter(|data| !data.is_empty()))
        }
        _ => {
            let _ = std::fs::remove_file(&output);
            Ok(None)
        }
    }
}

fn is_image_class(class: &str) -> bool {
    let class = class.to_ascii_lowercase();
    ["pngf", "tiff", "jpeg", "8bps", "gif", "webp"]
        .iter()
        .any(|c| class.contains(c))
}

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"];

pub enum ClipboardContent {
    None,
    Image(ClipboardImage),
    ImagePath(String),
    TextPath(String),
    Text(String),
}

pub fn read_clipboard() -> Result<ClipboardContent> {
    let classes = clipboard_classes()?;
    let has_file_url = classes
        .iter()
        .any(|c| c.contains("furl") || c.contains("file-url"));
    let has_image = classes.iter().any(|class| is_image_class(class));
    let has_text = classes
        .iter()
        .any(|c| c.contains("utf8") || c.contains("text"));
    if has_file_url || has_text {
        if let Some(text) = read_clipboard_text()? {
            if has_file_url || text.starts_with("file://") || text.starts_with('/') {
                if let Some(cp) = parse_clipboard_path(&text) {
                    if cp.is_image {
                        return Ok(ClipboardContent::ImagePath(cp.path));
                    } else {
                        return Ok(ClipboardContent::TextPath(cp.path));
                    }
                }
            }
            if has_text {
                return Ok(ClipboardContent::Text(text));
            }
        }
    }
    if has_image {
        if let Some(img) = read_clipboard_image()? {
            return Ok(ClipboardContent::Image(img));
        }
    }
    Ok(ClipboardContent::None)
}

/// macOS 剪贴板内容类型:`osascript -e 'clipboard info'` 输出
/// `«class PNGf», «class TIFF», «class utf8»` 之类;解析出类型名列表。
fn clipboard_classes() -> Result<Vec<String>> {
    let output = Command::new("osascript")
        .args(["-e", "clipboard info"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return Ok(Vec::new());
    };
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut classes = Vec::new();
    for token in text.split([',', '«', '»']) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if token.starts_with("class ") {
            classes.push(token.trim_start_matches("class ").to_string());
        } else if token.contains("class") {
            classes.push(token.to_string());
        }
    }
    Ok(classes)
}

pub struct ClipboardPath {
    pub path: String,
    pub is_image: bool,
}

pub fn read_clipboard_text() -> Result<Option<String>> {
    try_text_command("pbpaste", &[])
}

pub fn write_clipboard_text(text: &str) -> Result<bool> {
    try_write_text_command("pbcopy", &[], text)
}

fn try_write_text_command(cmd: &str, args: &[&str], text: &str) -> Result<bool> {
    let mut child = match Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return Ok(false),
    };

    if let Some(stdin) = &mut child.stdin {
        stdin.write_all(text.as_bytes())?;
    }
    Ok(child.wait().map(|status| status.success()).unwrap_or(false))
}

fn try_text_command(cmd: &str, args: &[&str]) -> Result<Option<String>> {
    let output = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => {
            let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(text))
            }
        }
        _ => Ok(None),
    }
}

pub fn parse_clipboard_path(text: &str) -> Option<ClipboardPath> {
    let text = text.trim();
    if text.is_empty() || text.contains('\n') || text.contains('\r') {
        return None;
    }
    let raw = text.strip_prefix("file://").unwrap_or(text);
    let raw = if text.starts_with("file://") {
        urlencoding::decode(raw)
            .map(|s| s.into_owned())
            .unwrap_or_else(|_| raw.to_string())
    } else {
        raw.to_string()
    };
    let path_str = if raw.starts_with('/') {
        raw.to_string()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
            home.join(rest).display().to_string()
        } else {
            return None;
        }
    } else {
        return None;
    };
    let path = Path::new(&path_str);
    if !path.exists() {
        return None;
    }
    let is_image = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false);
    Some(ClipboardPath {
        path: path_str,
        is_image,
    })
}

pub fn cleanup_clipboard_images(dir: &Path) {
    cleanup_clipboard_images_with_max(dir, CLIPBOARD_CACHE_MAX_BYTES);
}

fn cleanup_image_cache_throttled(dir: &Path) {
    let should_cleanup = LAST_IMAGE_CACHE_CLEANUP
        .lock()
        .map(|mut cleanups| {
            let now = Instant::now();
            let due = cleanups
                .get(dir)
                .map(|previous| now.duration_since(*previous) >= CLIPBOARD_CLEANUP_INTERVAL)
                .unwrap_or(true);
            if due {
                cleanups.insert(dir.to_path_buf(), now);
            }
            due
        })
        .unwrap_or(true);
    if should_cleanup {
        cleanup_clipboard_images_with_max(dir, CLIPBOARD_CACHE_MAX_BYTES);
    }
}

fn cleanup_clipboard_images_with_max(dir: &Path, max_bytes: u64) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut total: u64 = 0;

    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() && !meta.file_type().is_symlink() {
            continue;
        }
        let size = meta.len();
        let atime = meta.accessed().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        total += size;
        files.push((path, size, atime));
    }

    if total <= max_bytes {
        return;
    }

    files.sort_by(|a, b| a.2.cmp(&b.2));

    for (path, size, _) in &files {
        if total <= max_bytes {
            break;
        }
        let _ = std::fs::remove_file(path);
        total -= size;
    }
}
