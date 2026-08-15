//! renderer — 自 src/platforms/plugins/renderer.rs 拆分。

use super::*;

use anyhow::{anyhow, bail, Context, Result};
use cosmic_text::{
    Align as TextAlign, Attrs, Buffer, Color, Family, FontSystem, LayoutGlyph, Metrics, Shaping,
    Style as FontStyle, SwashCache, Weight, Wrap,
};
use fontdb::Database as FontDatabase;
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, Pixel as _, Rgba, RgbaImage};
use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const MAX_INPUT_CHARS: usize = 20_000;
pub(crate) const MAX_PAGE_PIXELS: u64 = 20_000_000;
pub(crate) const MAX_TOTAL_PAGE_PIXELS: u64 = 48_000_000;
pub(crate) const MAX_PAGE_PNG_BYTES: usize = 20 * 1024 * 1024;
pub(crate) const MAX_TOTAL_PNG_BYTES: usize = 48 * 1024 * 1024;
pub(crate) const MIN_CONFIGURED_HEIGHT: u32 = 1000;
pub(crate) const MIN_RENDERED_HEIGHT: u32 = 360;
pub(crate) const MAX_PAGE_HEIGHT: u32 = 5000;
pub(crate) const MAX_CACHED_GLYPHS: usize = 2048;
pub(crate) const MAX_CUSTOM_FONT_FILES: usize = 8;
pub(crate) const COLUMN_WIDTH: u32 = 960;
pub(crate) const COLUMN_GAP: u32 = 32;
pub(crate) const TARGET_ASPECT_RATIO: f32 = 4.0 / 3.0;
pub(crate) const ASPECT_TIE_EPSILON: f32 = 0.01;
pub(crate) const TABLE_CELL_PADDING: u32 = 14;
pub(crate) const WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
pub(crate) const RENDER_TIMEOUT: Duration = Duration::from_secs(60);
// debug 二进制未优化可到 550MB+,光映射自身就会撞 512MB 上限,worker
// 秒死只留下一句 "communication failed"——开发构建放宽到 2GB。
#[cfg(not(debug_assertions))]
pub(crate) const WORKER_ADDRESS_SPACE_LIMIT: u64 = 512 * 1024 * 1024;
#[cfg(debug_assertions)]
pub(crate) const WORKER_ADDRESS_SPACE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
pub(crate) const MAX_REQUEST_FRAME_BYTES: usize = 512 * 1024;
pub(crate) const MAX_ERROR_FRAME_BYTES: usize = 64 * 1024;
pub(crate) const MAX_RESPONSE_IMAGES: usize = 1;
pub(crate) const WORKER_ENV: &str = "GQY_INTERNAL_RENDERER_WORKER";
pub(crate) const WORKER_ARG: &str = "__renderer-worker";
pub(crate) const DEFAULT_BODY_FONT: &str = "Noto Sans CJK SC";
pub(crate) const DEFAULT_CODE_FONT: &str = "Noto Sans Mono CJK SC";
pub(crate) const DEFAULT_EMOJI_FONT: &str = "Noto Color Emoji";
pub(crate) const CJK_FONT_FILE: &str = "NotoSansCJK-Regular.ttc";
pub(crate) const EMOJI_FONT_FILE: &str = "NotoColorEmoji.ttf";
pub(crate) const RENDERER_FONTS_ENV: &str = "GQY_RENDERER_FONTS_DIR";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RenderConfig {
    pub(crate) theme: String,
    pub(crate) max_height: u32,
    pub(crate) font_size: u32,
    pub(crate) code_font_size: u32,
    pub(crate) padding: u32,
    pub(crate) font: String,
    pub(crate) title_font: String,
    pub(crate) code_font: String,
    pub(crate) emoji_font: String,
}

impl Default for RenderConfig {
    pub(crate) fn default() -> Self {
        Self {
            theme: "paper".to_string(),
            max_height: 2600,
            font_size: 36,
            code_font_size: 30,
            padding: 64,
            font: String::new(),
            title_font: String::new(),
            code_font: String::new(),
            emoji_font: String::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct RenderedImage {
    pub(crate) mime: String,
    pub(crate) png: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Clone)]
pub(crate) struct MarkdownImageRenderer {
    worker: Arc<Mutex<WorkerSlot>>,
}

pub(crate) struct RendererState {
    font_system: FontSystem,
    swash_cache: SwashCache,
    resolved_fonts: HashMap<String, Option<String>>,
    emoji_font_path: PathBuf,
    emoji_loaded: bool,
}

impl MarkdownImageRenderer {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            worker: Arc::new(Mutex::new(WorkerSlot::default())),
        })
    }

    pub(crate) async fn render(
        &self,
        markdown: &str,
        config: &RenderConfig,
    ) -> Result<Vec<RenderedImage>> {
        validate_markdown(markdown)?;
        #[cfg(test)]
        {
            render_in_process_for_test(markdown, config)
        }
        #[cfg(not(test))]
        {
            self.render_with_worker(markdown, config).await
        }
    }

    #[cfg(not(test))]
    pub(crate) async fn render_with_worker(
        &self,
        markdown: &str,
        config: &RenderConfig,
    ) -> Result<Vec<RenderedImage>> {
        let request = RenderRequest {
            markdown: markdown.to_string(),
            config: config.clone(),
        };
        let mut slot = self.worker.lock().await;
        slot.cancel_idle_timer();

        for attempt in 0..2 {
            let mut worker = match slot.process.take() {
                Some(worker) => worker,
                None => WorkerProcess::spawn().await?,
            };
            let result =
                tokio::time::timeout(RENDER_TIMEOUT, exchange_with_worker(&mut worker, &request))
                    .await;
            match result {
                Ok(Ok(images)) => {
                    self.recycle_worker(&mut slot, worker);
                    return Ok(images);
                }
                Ok(Err(WorkerExchangeError::Render(message))) => {
                    self.recycle_worker(&mut slot, worker);
                    return Err(anyhow!(
                        "long-image renderer rejected the request: {message}"
                    ));
                }
                Ok(Err(WorkerExchangeError::Transport(error))) => {
                    stop_worker(worker).await;
                    if attempt == 1 {
                        return Err(error)
                            .context("long-image renderer worker communication failed");
                    }
                }
                Err(_) => {
                    stop_worker(worker).await;
                    bail!(
                        "long-image renderer exceeded its {}-second timeout",
                        RENDER_TIMEOUT.as_secs()
                    );
                }
            }
        }
        unreachable!("renderer worker retry loop always returns")
    }

    #[cfg(not(test))]
    pub(crate) fn recycle_worker(&self, slot: &mut WorkerSlot, worker: WorkerProcess) {
        slot.process = Some(worker);
        slot.generation = slot.generation.wrapping_add(1);
        let generation = slot.generation;
        let weak_slot = Arc::downgrade(&self.worker);
        slot.idle_task = Some(tokio::spawn(async move {
            tokio::time::sleep(WORKER_IDLE_TIMEOUT).await;
            let Some(shared_slot) = weak_slot.upgrade() else {
                return;
            };
            let mut slot = shared_slot.lock().await;
            if slot.generation != generation {
                return;
            }
            if let Some(worker) = slot.process.take() {
                stop_worker(worker).await;
            }
            slot.idle_task.take();
        }));
    }
}

#[cfg(test)]
pub(crate) fn render_in_process_for_test(
    markdown: &str,
    raw_config: &RenderConfig,
) -> Result<Vec<RenderedImage>> {
    pub(crate) static RENDERER: std::sync::OnceLock<std::sync::Mutex<RendererState>> =
        std::sync::OnceLock::new();
    let renderer = RENDERER.get_or_init(|| std::sync::Mutex::new(RendererState::new().unwrap()));
    let mut renderer = renderer.lock().unwrap();
    validate_markdown(markdown)?;
    let config = NormalizedConfig::new(raw_config);
    let blocks = collect_blocks(markdown);
    let palette = Palette::for_theme(&config.theme);
    renderer.render(blocks, &config, palette, markdown_contains_emoji(markdown))
}

pub(crate) struct WorkerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

#[derive(Default)]
pub(crate) struct WorkerSlot {
    process: Option<WorkerProcess>,
    idle_task: Option<tokio::task::JoinHandle<()>>,
    generation: u64,
}

impl WorkerSlot {
    pub(crate) fn cancel_idle_timer(&mut self) {
        if let Some(task) = self.idle_task.take() {
            task.abort();
        }
    }
}

impl Drop for WorkerSlot {
    pub(crate) fn drop(&mut self) {
        self.cancel_idle_timer();
    }
}

impl WorkerProcess {
    pub(crate) async fn spawn() -> Result<Self> {
        let executable = std::env::current_exe().context("locating the GQY executable")?;
        let mut command = tokio::process::Command::new(executable);
        command
            .arg(WORKER_ARG)
            .env(WORKER_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .context("starting the long-image renderer worker")?;
        let stdin = child
            .stdin
            .take()
            .context("renderer worker stdin was not piped")?;
        let stdout = child
            .stdout
            .take()
            .context("renderer worker stdout was not piped")?;
        Ok(Self {
            child,
            stdin,
            stdout,
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct RenderRequest {
    markdown: String,
    config: RenderConfig,
}

#[derive(Debug)]
pub(crate) enum WorkerExchangeError {
    Transport(anyhow::Error),
    Render(String),
}

pub(crate) async fn exchange_with_worker(
    worker: &mut WorkerProcess,
    request: &RenderRequest,
) -> std::result::Result<Vec<RenderedImage>, WorkerExchangeError> {
    let payload = serde_json::to_vec(request)
        .map_err(|error| WorkerExchangeError::Transport(error.into()))?;
    write_frame(&mut worker.stdin, &payload)
        .await
        .map_err(WorkerExchangeError::Transport)?;
    tokio::io::AsyncWriteExt::flush(&mut worker.stdin)
        .await
        .map_err(|error| WorkerExchangeError::Transport(error.into()))?;
    read_worker_response(&mut worker.stdout).await
}

pub(crate) async fn stop_worker(mut worker: WorkerProcess) {
    let _ = worker.child.kill().await;
    let _ = worker.child.wait().await;
}

pub(crate) fn renderer_worker_requested() -> bool {
    std::env::var_os(WORKER_ENV).as_deref() == Some(std::ffi::OsStr::new("1"))
        && std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new(WORKER_ARG))
}

pub(crate) async fn run_renderer_worker() -> Result<()> {
    apply_worker_address_space_limit()?;
    let mut renderer = RendererState::new()?;
    let mut input = tokio::io::stdin();
    let mut output = tokio::io::stdout();

    loop {
        let payload = match tokio::time::timeout(
            WORKER_IDLE_TIMEOUT,
            read_frame(&mut input, MAX_REQUEST_FRAME_BYTES),
        )
        .await
        {
            Err(_) => return Ok(()),
            Ok(Ok(Some(payload))) => payload,
            Ok(Ok(None)) => return Ok(()),
            Ok(Err(error)) => return Err(error),
        };
        let result = serde_json::from_slice::<RenderRequest>(&payload)
            .context("decoding the renderer request")
            .and_then(|request| {
                validate_markdown(&request.markdown)?;
                let config = NormalizedConfig::new(&request.config);
                let blocks = collect_blocks(&request.markdown);
                let palette = Palette::for_theme(&config.theme);
                renderer.render(
                    blocks,
                    &config,
                    palette,
                    markdown_contains_emoji(&request.markdown),
                )
            });
        write_worker_response(&mut output, result).await?;
        tokio::io::AsyncWriteExt::flush(&mut output).await?;
    }
}

#[cfg(unix)]
pub(crate) fn apply_worker_address_space_limit() -> Result<()> {
    let limit = libc::rlimit {
        rlim_cur: WORKER_ADDRESS_SPACE_LIMIT as libc::rlim_t,
        rlim_max: WORKER_ADDRESS_SPACE_LIMIT as libc::rlim_t,
    };
    if unsafe { libc::setrlimit(libc::RLIMIT_AS, &limit) } != 0 {
        return Err(io::Error::last_os_error()).context("limiting renderer worker address space");
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn apply_worker_address_space_limit() -> Result<()> {
    Ok(())
}

impl RendererState {
    pub(crate) fn new() -> Result<Self> {
        Self::from_font_dir(&renderer_fonts_dir()?)
    }

    pub(crate) fn from_font_dir(font_dir: &std::path::Path) -> Result<Self> {
        let mut database = FontDatabase::new();
        let cjk_font = font_dir.join(CJK_FONT_FILE);
        database
            .load_font_file(&cjk_font)
            .with_context(|| format!("loading renderer font {}", cjk_font.display()))?;
        if database.faces().next().is_none() {
            bail!("renderer font {} contains no faces", cjk_font.display());
        }
        database.set_sans_serif_family(DEFAULT_BODY_FONT);
        database.set_monospace_family(DEFAULT_CODE_FONT);
        Ok(Self {
            font_system: FontSystem::new_with_locale_and_db("zh-CN".to_string(), database),
            swash_cache: SwashCache::new(),
            resolved_fonts: HashMap::new(),
            emoji_font_path: font_dir.join(EMOJI_FONT_FILE),
            emoji_loaded: false,
        })
    }
}

pub(crate) fn renderer_fonts_dir() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os(RENDERER_FONTS_ENV) {
        candidates.push(PathBuf::from(path));
    }
    #[cfg(debug_assertions)]
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"));
    candidates.push(PathBuf::from("/usr/share/gqy/fonts"));
    if let Ok(executable) = std::env::current_exe() {
        if let Some(prefix) = executable.parent().and_then(std::path::Path::parent) {
            candidates.push(prefix.join("share/gqy/fonts"));
        }
        if let Some(workspace) = executable
            .parent()
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::parent)
        {
            candidates.push(workspace.join("assets/fonts"));
        }
    }
    // 兜底:发行版 noto-fonts-cjk 的标准安装路径。gqy 专用字体目录缺失
    // (比如误装了不带字体的 release 资产包)时,长文转图靠系统字体继续工作。
    candidates.push(PathBuf::from("/usr/share/fonts/noto-cjk"));
    for candidate in &candidates {
        if candidate.join(CJK_FONT_FILE).is_file() {
            return Ok(candidate.clone());
        }
    }
    let searched = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "renderer font is missing; install {CJK_FONT_FILE} in /usr/share/gqy/fonts or set {RENDERER_FONTS_ENV} (searched: {searched})"
    )
}

pub(crate) async fn write_frame<W>(writer: &mut W, payload: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if payload.len() > MAX_REQUEST_FRAME_BYTES {
        bail!("renderer request frame exceeds the {MAX_REQUEST_FRAME_BYTES}-byte limit");
    }
    let length = u32::try_from(payload.len()).context("renderer request frame is too large")?;
    tokio::io::AsyncWriteExt::write_all(writer, &length.to_be_bytes()).await?;
    tokio::io::AsyncWriteExt::write_all(writer, payload).await?;
    Ok(())
}

pub(crate) async fn read_frame<R>(reader: &mut R, limit: usize) -> Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let mut length = [0_u8; 4];
    match tokio::io::AsyncReadExt::read_exact(reader, &mut length).await {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let length = u32::from_be_bytes(length) as usize;
    if length > limit {
        bail!("renderer frame exceeds the {limit}-byte limit");
    }
    let mut payload = vec![0_u8; length];
    tokio::io::AsyncReadExt::read_exact(reader, &mut payload).await?;
    Ok(Some(payload))
}

pub(crate) async fn write_worker_response<W>(writer: &mut W, result: Result<Vec<RenderedImage>>) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    match result {
        Ok(images) => {
            if images.len() > MAX_RESPONSE_IMAGES {
                bail!("renderer returned more than {MAX_RESPONSE_IMAGES} image");
            }
            tokio::io::AsyncWriteExt::write_all(writer, &[0]).await?;
            write_u32(writer, images.len(), "renderer image count").await?;
            for image in images {
                validate_page_dimensions(image.width, image.height)?;
                if image.png.len() > MAX_PAGE_PNG_BYTES {
                    bail!("renderer returned a PNG larger than its configured limit");
                }
                write_u32_value(writer, image.width).await?;
                write_u32_value(writer, image.height).await?;
                write_sized_bytes(writer, image.mime.as_bytes(), 64, "renderer MIME type").await?;
                write_sized_bytes(
                    writer,
                    &image.png,
                    MAX_PAGE_PNG_BYTES,
                    "renderer PNG payload",
                )
                .await?;
            }
        }
        Err(error) => {
            tokio::io::AsyncWriteExt::write_all(writer, &[1]).await?;
            let mut message = format!("{error:#}");
            if message.len() > MAX_ERROR_FRAME_BYTES {
                let mut end = MAX_ERROR_FRAME_BYTES;
                while !message.is_char_boundary(end) {
                    end = end.saturating_sub(1);
                }
                message.truncate(end);
            }
            write_sized_bytes(
                writer,
                message.as_bytes(),
                MAX_ERROR_FRAME_BYTES,
                "renderer error",
            )
            .await?;
        }
    }
    Ok(())
}

pub(crate) async fn read_worker_response<R>(
    reader: &mut R,
) -> std::result::Result<Vec<RenderedImage>, WorkerExchangeError>
where
    R: AsyncRead + Unpin,
{
    let status = read_byte(reader)
        .await
        .map_err(WorkerExchangeError::Transport)?;
    match status {
        0 => {
            let count = read_u32(reader)
                .await
                .map_err(WorkerExchangeError::Transport)? as usize;
            if count > MAX_RESPONSE_IMAGES {
                return Err(WorkerExchangeError::Transport(anyhow!(
                    "renderer response contains too many images"
                )));
            }
            let mut images = Vec::with_capacity(count);
            let mut total_png_bytes = 0_usize;
            for _ in 0..count {
                let width = read_u32(reader)
                    .await
                    .map_err(WorkerExchangeError::Transport)?;
                let height = read_u32(reader)
                    .await
                    .map_err(WorkerExchangeError::Transport)?;
                validate_page_dimensions(width, height).map_err(WorkerExchangeError::Transport)?;
                let mime = read_sized_bytes(reader, 64)
                    .await
                    .map_err(WorkerExchangeError::Transport)?;
                let mime = String::from_utf8(mime)
                    .context("renderer returned a non-UTF-8 MIME type")
                    .map_err(WorkerExchangeError::Transport)?;
                let png = read_sized_bytes(reader, MAX_PAGE_PNG_BYTES)
                    .await
                    .map_err(WorkerExchangeError::Transport)?;
                total_png_bytes = total_png_bytes
                    .checked_add(png.len())
                    .context("renderer PNG byte count overflowed")
                    .map_err(WorkerExchangeError::Transport)?;
                if total_png_bytes > MAX_TOTAL_PNG_BYTES {
                    return Err(WorkerExchangeError::Transport(anyhow!(
                        "renderer response exceeds the total PNG byte limit"
                    )));
                }
                images.push(RenderedImage {
                    mime,
                    png,
                    width,
                    height,
                });
            }
            Ok(images)
        }
        1 => {
            let message = read_sized_bytes(reader, MAX_ERROR_FRAME_BYTES)
                .await
                .map_err(WorkerExchangeError::Transport)?;
            let message = String::from_utf8_lossy(&message).into_owned();
            Err(WorkerExchangeError::Render(message))
        }
        value => Err(WorkerExchangeError::Transport(anyhow!(
            "renderer response has unknown status byte {value}"
        ))),
    }
}

pub(crate) async fn write_u32<W>(writer: &mut W, value: usize, label: &str) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let value = u32::try_from(value).with_context(|| format!("{label} does not fit in u32"))?;
    write_u32_value(writer, value).await
}

pub(crate) async fn write_u32_value<W>(writer: &mut W, value: u32) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    tokio::io::AsyncWriteExt::write_all(writer, &value.to_be_bytes()).await?;
    Ok(())
}

pub(crate) async fn read_u32<R>(reader: &mut R) -> Result<u32>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = [0_u8; 4];
    tokio::io::AsyncReadExt::read_exact(reader, &mut bytes).await?;
    Ok(u32::from_be_bytes(bytes))
}

pub(crate) async fn read_byte<R>(reader: &mut R) -> Result<u8>
where
    R: AsyncRead + Unpin,
{
    let mut byte = [0_u8; 1];
    tokio::io::AsyncReadExt::read_exact(reader, &mut byte).await?;
    Ok(byte[0])
}

pub(crate) async fn write_sized_bytes<W>(writer: &mut W, bytes: &[u8], limit: usize, label: &str) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if bytes.len() > limit {
        bail!("{label} exceeds the {limit}-byte limit");
    }
    write_u32(writer, bytes.len(), label).await?;
    tokio::io::AsyncWriteExt::write_all(writer, bytes).await?;
    Ok(())
}

pub(crate) async fn read_sized_bytes<R>(reader: &mut R, limit: usize) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let length = read_u32(reader).await? as usize;
    if length > limit {
        bail!("renderer response field exceeds the {limit}-byte limit");
    }
    let mut bytes = vec![0_u8; length];
    tokio::io::AsyncReadExt::read_exact(reader, &mut bytes).await?;
    Ok(bytes)
}

impl RendererState {
    pub(crate) fn render(
        &mut self,
        blocks: Vec<Block>,
        config: &NormalizedConfig,
        palette: Palette,
        needs_emoji: bool,
    ) -> Result<Vec<RenderedImage>> {
        if self.swash_cache.image_cache.len() > MAX_CACHED_GLYPHS {
            self.swash_cache.image_cache.clear();
        }
        let fonts = self.resolve_config_fonts(config, needs_emoji)?;
        let layouts = layout_blocks(&mut self.font_system, blocks, config, palette, &fonts)?;
        let columns = plan_balanced_columns(&layouts, config)?;
        let rendered = render_pages(
            &mut self.font_system,
            &mut self.swash_cache,
            &layouts,
            &columns,
            config,
            palette,
        );
        if self.swash_cache.image_cache.len() > MAX_CACHED_GLYPHS {
            self.swash_cache.image_cache.clear();
        }
        rendered
    }

    pub(crate) fn resolve_config_fonts(
        &mut self,
        config: &NormalizedConfig,
        needs_emoji: bool,
    ) -> Result<ResolvedFonts> {
        let body = self
            .resolve_font(&config.font)
            .or_else(|| Some(DEFAULT_BODY_FONT.to_string()));
        let title = if config.title_font.trim().is_empty() {
            body.clone()
        } else {
            self.resolve_font(&config.title_font)
        };
        let emoji = if needs_emoji {
            let configured = config.emoji_font.trim();
            if configured.is_empty() || configured.eq_ignore_ascii_case(DEFAULT_EMOJI_FONT) {
                self.ensure_bundled_emoji_font()?;
                Some(DEFAULT_EMOJI_FONT.to_string())
            } else if let Some(font) = self.resolve_font(configured) {
                Some(font)
            } else {
                self.ensure_bundled_emoji_font()?;
                Some(DEFAULT_EMOJI_FONT.to_string())
            }
        } else {
            None
        };
        Ok(ResolvedFonts {
            body,
            title,
            code: self
                .resolve_font(&config.code_font)
                .or_else(|| Some(DEFAULT_CODE_FONT.to_string())),
            emoji,
        })
    }

    pub(crate) fn ensure_bundled_emoji_font(&mut self) -> Result<()> {
        if self.emoji_loaded {
            return Ok(());
        }

        let previous_faces = self.font_system.db().faces().count();
        self.font_system
            .db_mut()
            .load_font_file(&self.emoji_font_path)
            .with_context(|| {
                format!(
                    "loading renderer Emoji font {}",
                    self.emoji_font_path.display()
                )
            })?;
        let has_emoji_family = self
            .font_system
            .db()
            .faces()
            .skip(previous_faces)
            .any(|face| {
                face.families
                    .iter()
                    .any(|(family, _)| family.eq_ignore_ascii_case(DEFAULT_EMOJI_FONT))
            });
        if !has_emoji_family {
            bail!(
                "renderer Emoji font {} does not contain the {DEFAULT_EMOJI_FONT} family",
                self.emoji_font_path.display()
            );
        }
        self.emoji_loaded = true;
        Ok(())
    }

    pub(crate) fn resolve_font(&mut self, configured: &str) -> Option<String> {
        let configured = configured.trim();
        if configured.is_empty() {
            return None;
        }
        let path = PathBuf::from(configured);
        if !path.is_file() {
            let bundled_family = self.font_system.db().faces().any(|face| {
                face.families
                    .iter()
                    .any(|(family, _)| family.eq_ignore_ascii_case(configured))
            });
            if !bundled_family {
                tracing::warn!(
                    font = configured,
                    "{}",
                    crate::i18n::text(
                        "long-image renderer font is not a bundled family or readable file; using the default font",
                        "长图渲染器字体不是内置字体族或可读文件；使用默认字体"
                    )
                );
                return None;
            }
            return Some(configured.to_string());
        }
        let path = path.canonicalize().unwrap_or(path);
        let cache_key = path.to_string_lossy().into_owned();
        if let Some(cached) = self.resolved_fonts.get(&cache_key) {
            return cached.clone();
        }
        if self.resolved_fonts.len() >= MAX_CUSTOM_FONT_FILES {
            tracing::warn!(
                font = %path.display(),
                limit = MAX_CUSTOM_FONT_FILES,
                "{}",
                crate::i18n::text(
                    "long-image renderer custom font limit reached; using the default font",
                    "长图渲染器已达到自定义字体上限；使用默认字体"
                )
            );
            return None;
        }

        let previous_faces = self.font_system.db().faces().count();
        let resolved = self
            .font_system
            .db_mut()
            .load_font_file(&path)
            .ok()
            .and_then(|()| {
                self.font_system
                    .db()
                    .faces()
                    .skip(previous_faces)
                    .find_map(|face| face.families.first().map(|(name, _)| name.clone()))
            });
        self.resolved_fonts.insert(cache_key, resolved.clone());
        resolved
    }
}

#[derive(Clone)]
pub(crate) struct NormalizedConfig {
    theme: String,
    max_height: u32,
    font_size: u32,
    code_font_size: u32,
    padding: u32,
    font: String,
    title_font: String,
    code_font: String,
    emoji_font: String,
}

impl NormalizedConfig {
    pub(crate) fn new(config: &RenderConfig) -> Self {
        Self {
            theme: config.theme.trim().to_ascii_lowercase(),
            max_height: config
                .max_height
                .clamp(MIN_CONFIGURED_HEIGHT, MAX_PAGE_HEIGHT),
            font_size: config.font_size.clamp(14, 56),
            code_font_size: config.code_font_size.clamp(12, 52),
            padding: config.padding.clamp(24, 160),
            font: config.font.clone(),
            title_font: config.title_font.clone(),
            code_font: config.code_font.clone(),
            emoji_font: config.emoji_font.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ResolvedFonts {
    body: Option<String>,
    title: Option<String>,
    code: Option<String>,
    emoji: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) struct Palette {
    background: [u8; 4],
    text: [u8; 4],
    heading: [u8; 4],
    muted: [u8; 4],
    link: [u8; 4],
    code_background: [u8; 4],
    code_text: [u8; 4],
    quote_background: [u8; 4],
    quote_bar: [u8; 4],
    table_header_background: [u8; 4],
    table_background: [u8; 4],
    border: [u8; 4],
    rule: [u8; 4],
}

impl Palette {
    pub(crate) fn for_theme(theme: &str) -> Self {
        match theme {
            "dark" => Self {
                background: [28, 29, 32, 255],
                text: [231, 232, 235, 255],
                heading: [255, 255, 255, 255],
                muted: [164, 168, 176, 255],
                link: [104, 179, 255, 255],
                code_background: [43, 45, 51, 255],
                code_text: [239, 240, 244, 255],
                quote_background: [37, 40, 45, 255],
                quote_bar: [93, 168, 143, 255],
                table_header_background: [19, 20, 23, 255],
                table_background: [34, 36, 40, 255],
                border: [72, 76, 84, 255],
                rule: [83, 87, 95, 255],
            },
            "light" => Self {
                background: [250, 250, 248, 255],
                text: [30, 34, 40, 255],
                heading: [18, 20, 24, 255],
                muted: [92, 96, 104, 255],
                link: [48, 101, 190, 255],
                code_background: [226, 229, 235, 255],
                code_text: [34, 38, 45, 255],
                quote_background: [244, 247, 255, 255],
                quote_bar: [74, 116, 214, 255],
                table_header_background: [238, 240, 244, 255],
                table_background: [246, 247, 249, 255],
                border: [218, 222, 230, 255],
                rule: [218, 222, 230, 255],
            },
            _ => Self {
                background: [244, 239, 229, 255],
                text: [48, 46, 41, 255],
                heading: [37, 34, 29, 255],
                muted: [104, 98, 88, 255],
                link: [112, 82, 43, 255],
                code_background: [225, 219, 208, 255],
                code_text: [42, 39, 34, 255],
                quote_background: [236, 229, 214, 255],
                quote_bar: [134, 101, 54, 255],
                table_header_background: [232, 226, 215, 255],
                table_background: [239, 233, 222, 255],
                border: [211, 201, 184, 255],
                rule: [211, 201, 184, 255],
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockKind {
    Paragraph,
    Heading(u8),
    ListItem { depth: u8 },
    Quote,
    Code,
    Table,
    Rule,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InlineStyle {
    bold: bool,
    italic: bool,
    code: bool,
    link: bool,
    muted: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct RichSpan {
    text: String,
    style: InlineStyle,
}

#[derive(Clone, Debug)]
pub(crate) struct Block {
    kind: BlockKind,
    spans: Vec<RichSpan>,
    table: Option<TableBlock>,
    task: Option<bool>,
}

impl Block {
    pub(crate) fn new(kind: BlockKind) -> Self {
        Self {
            kind,
            spans: Vec::new(),
            table: None,
            task: None,
        }
    }

    pub(crate) fn push(&mut self, text: &str, style: InlineStyle) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.spans.last_mut().filter(|last| last.style == style) {
            last.text.push_str(text);
        } else {
            self.spans.push(RichSpan {
                text: text.to_string(),
                style,
            });
        }
    }

    pub(crate) fn has_content(&self) -> bool {
        self.kind == BlockKind::Rule
            || self.spans.iter().any(|span| !span.text.is_empty())
            || self.table.as_ref().is_some_and(TableBlock::has_content)
            || self.task.is_some()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TableBlock {
    alignments: Vec<Alignment>,
    header: Vec<Vec<RichSpan>>,
    rows: Vec<Vec<Vec<RichSpan>>>,
}

impl TableBlock {
    pub(crate) fn has_content(&self) -> bool {
        !self.header.is_empty() || !self.rows.is_empty()
    }
}

#[derive(Default)]
pub(crate) struct TableBuilder {
    alignments: Vec<Alignment>,
    header: Vec<Vec<RichSpan>>,
    rows: Vec<Vec<Vec<RichSpan>>>,
    current_row: Vec<Vec<RichSpan>>,
    current_cell: Vec<RichSpan>,
    in_cell: bool,
}

impl TableBuilder {
    pub(crate) fn push(&mut self, text: &str, style: InlineStyle) {
        if text.is_empty() || !self.in_cell {
            return;
        }
        if let Some(last) = self
            .current_cell
            .last_mut()
            .filter(|last| last.style == style)
        {
            last.text.push_str(text);
        } else {
            self.current_cell.push(RichSpan {
                text: text.to_string(),
                style,
            });
        }
    }

    pub(crate) fn start_row(&mut self) {
        self.current_row.clear();
        self.current_cell.clear();
        self.in_cell = false;
    }

    pub(crate) fn start_cell(&mut self) {
        self.current_cell.clear();
        self.in_cell = true;
    }

    pub(crate) fn finish_cell(&mut self) {
        if self.in_cell {
            self.current_row
                .push(std::mem::take(&mut self.current_cell));
            self.in_cell = false;
        }
    }

    pub(crate) fn finish_row(&mut self, header: bool) {
        self.finish_cell();
        let row = std::mem::take(&mut self.current_row);
        if row.is_empty() {
            return;
        }
        if header {
            self.header = row;
        } else {
            self.rows.push(row);
        }
    }

    pub(crate) fn finish(mut self) -> TableBlock {
        self.finish_cell();
        TableBlock {
            alignments: self.alignments,
            header: self.header,
            rows: self.rows,
        }
    }
}

pub(crate) struct ListState {
    ordered: bool,
    next: u64,
    in_item: bool,
    prefix_used: bool,
}

#[derive(Default)]
pub(crate) struct MarkdownCollector {
    blocks: Vec<Block>,
    current: Option<Block>,
    lists: Vec<ListState>,
    quote_depth: usize,
    heading: Option<u8>,
    code_block: bool,
    table: Option<TableBuilder>,
    table_header: bool,
    strong_depth: usize,
    emphasis_depth: usize,
    link_depth: usize,
    strike_depth: usize,
}

impl MarkdownCollector {
    pub(crate) fn collect(mut self, markdown: &str) -> Vec<Block> {
        let options =
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        for event in Parser::new_ext(markdown, options) {
            self.event(event);
        }
        self.finish_current();
        self.blocks
    }

    pub(crate) fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text, self.style()),
            Event::Code(text) => {
                let mut style = self.style();
                style.code = true;
                self.push_text(&text, style);
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                let mut style = self.style();
                style.code = true;
                self.push_text(&text, style);
            }
            Event::SoftBreak => {
                let separator = if self.code_block || self.table.is_some() {
                    "\n"
                } else {
                    " "
                };
                self.push_text(separator, self.style());
            }
            Event::HardBreak => self.push_text("\n", self.style()),
            Event::Rule => {
                self.finish_current();
                self.blocks.push(Block::new(BlockKind::Rule));
            }
            Event::TaskListMarker(done) => {
                self.mark_task(done);
            }
            Event::FootnoteReference(label) => {
                self.push_text(&format!("[{label}]"), self.style());
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                self.push_text(&text, self.style());
            }
        }
    }

    pub(crate) fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.ensure_current(),
            Tag::Heading { level, .. } => {
                self.finish_current();
                let level = level as u8;
                self.heading = Some(level);
                self.current = Some(Block::new(BlockKind::Heading(level)));
            }
            Tag::BlockQuote(_) => {
                self.finish_current();
                self.quote_depth = self.quote_depth.saturating_add(1);
            }
            Tag::CodeBlock(_) => {
                self.finish_current();
                self.code_block = true;
                self.current = Some(Block::new(BlockKind::Code));
            }
            Tag::List(start) => {
                self.finish_current();
                self.lists.push(ListState {
                    ordered: start.is_some(),
                    next: start.unwrap_or(1),
                    in_item: false,
                    prefix_used: false,
                });
            }
            Tag::Item => {
                self.finish_current();
                if let Some(list) = self.lists.last_mut() {
                    list.in_item = true;
                    list.prefix_used = false;
                }
                self.ensure_current();
            }
            Tag::Table(alignments) => {
                self.finish_current();
                self.table = Some(TableBuilder {
                    alignments,
                    ..TableBuilder::default()
                });
                self.table_header = false;
            }
            Tag::TableHead => {
                self.table_header = true;
                if let Some(table) = self.table.as_mut() {
                    table.start_row();
                }
            }
            Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.start_row();
                }
            }
            Tag::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.start_cell();
                }
            }
            Tag::Strong => self.strong_depth = self.strong_depth.saturating_add(1),
            Tag::Emphasis => self.emphasis_depth = self.emphasis_depth.saturating_add(1),
            Tag::Strikethrough => self.strike_depth = self.strike_depth.saturating_add(1),
            Tag::Link { .. } | Tag::Image { .. } => {
                self.link_depth = self.link_depth.saturating_add(1)
            }
            _ => {}
        }
    }

    pub(crate) fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                if !self.code_block && self.table.is_none() && self.heading.is_none() {
                    self.finish_current();
                }
            }
            TagEnd::Heading(_) => {
                self.finish_current();
                self.heading = None;
            }
            TagEnd::BlockQuote(_) => {
                self.finish_current();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                self.finish_current();
                self.code_block = false;
            }
            TagEnd::List(_) => {
                self.finish_current();
                self.lists.pop();
            }
            TagEnd::Item => {
                self.finish_current();
                if let Some(list) = self.lists.last_mut() {
                    list.in_item = false;
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    let mut block = Block::new(BlockKind::Table);
                    block.table = Some(table.finish());
                    if block.has_content() {
                        self.blocks.push(block);
                    }
                }
                self.table_header = false;
            }
            TagEnd::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.finish_row(true);
                }
                self.table_header = false;
            }
            TagEnd::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.finish_row(self.table_header);
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.finish_cell();
                }
            }
            TagEnd::Strong => self.strong_depth = self.strong_depth.saturating_sub(1),
            TagEnd::Emphasis => self.emphasis_depth = self.emphasis_depth.saturating_sub(1),
            TagEnd::Strikethrough => self.strike_depth = self.strike_depth.saturating_sub(1),
            TagEnd::Link | TagEnd::Image => self.link_depth = self.link_depth.saturating_sub(1),
            _ => {}
        }
    }

    pub(crate) fn ensure_current(&mut self) {
        if self.current.is_some() {
            return;
        }
        if self.code_block {
            self.current = Some(Block::new(BlockKind::Code));
            return;
        }
        if self.table.is_some() {
            return;
        }
        if let Some(level) = self.heading {
            self.current = Some(Block::new(BlockKind::Heading(level)));
            return;
        }

        if let Some(index) = self.lists.iter().rposition(|list| list.in_item) {
            let depth = u8::try_from(index + 1).unwrap_or(u8::MAX);
            let list = &mut self.lists[index];
            let prefix = if list.prefix_used {
                "    ".to_string()
            } else if list.ordered {
                let number = list.next;
                list.next = list.next.saturating_add(1);
                list.prefix_used = true;
                format!("{number}. ")
            } else {
                list.prefix_used = true;
                "• ".to_string()
            };
            let mut block = Block::new(BlockKind::ListItem { depth });
            block.push(&prefix, InlineStyle::default());
            self.current = Some(block);
        } else if self.quote_depth > 0 {
            self.current = Some(Block::new(BlockKind::Quote));
        } else {
            self.current = Some(Block::new(BlockKind::Paragraph));
        }
    }

    pub(crate) fn push_text(&mut self, text: &str, style: InlineStyle) {
        if let Some(table) = self.table.as_mut() {
            table.push(text, style);
            return;
        }
        self.ensure_current();
        if let Some(block) = self.current.as_mut() {
            block.push(text, style);
        }
    }

    pub(crate) fn mark_task(&mut self, done: bool) {
        self.ensure_current();
        let Some(block) = self.current.as_mut() else {
            return;
        };
        if let Some(first) = block.spans.first_mut() {
            if let Some(rest) = first.text.strip_prefix("• ") {
                first.text = rest.to_string();
                if first.text.is_empty() {
                    block.spans.remove(0);
                }
            }
        }
        block.task = Some(done);
    }

    pub(crate) fn style(&self) -> InlineStyle {
        InlineStyle {
            bold: self.strong_depth > 0 || self.table_header,
            italic: self.emphasis_depth > 0,
            code: self.code_block,
            link: self.link_depth > 0,
            muted: self.strike_depth > 0,
        }
    }

    pub(crate) fn finish_current(&mut self) {
        let Some(block) = self.current.take() else {
            return;
        };
        if block.has_content() {
            self.blocks.push(block);
        }
    }
}

pub(crate) fn collect_blocks(markdown: &str) -> Vec<Block> {
    MarkdownCollector::default().collect(markdown)
}

pub(crate) fn validate_markdown(markdown: &str) -> Result<()> {
    let count = markdown.chars().take(MAX_INPUT_CHARS + 1).count();
    if count > MAX_INPUT_CHARS {
        bail!("Markdown image input exceeds the {MAX_INPUT_CHARS}-character limit");
    }
    Ok(())
}

pub(crate) struct LayoutBlock {
    kind: BlockKind,
    buffer: Option<Buffer>,
    table: Option<LayoutTable>,
    task: Option<TaskBox>,
    total_height: u32,
    vertical_padding: u32,
    inset_left: u32,
    boundaries: Vec<u32>,
    margin_before: u32,
    margin_after: u32,
    default_color: Color,
    inline_code_background: [u8; 4],
}

pub(crate) struct LayoutTable {
    rows: Vec<LayoutTableRow>,
    header_height: u32,
}

pub(crate) struct LayoutTableRow {
    cells: Vec<LayoutTableCell>,
    source_start: u32,
    source_end: u32,
    header: bool,
    stripe: bool,
}

pub(crate) struct LayoutTableCell {
    buffer: Buffer,
    x: u32,
    width: u32,
    default_color: Color,
    inline_code_background: [u8; 4],
}

