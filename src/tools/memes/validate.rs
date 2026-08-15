//! validate — 自 src/tools/memes.rs 拆分。

pub(crate) use super::*;

pub(crate) fn validate_classification(classification: &MemeClassification) -> Result<()> {
    if !classification.save {
        return Ok(());
    }
    if classification.confidence != 100 {
        bail!("accepted meme classification confidence must be exactly 100")
    }
    if !classification.positive_gates.chat_reaction
        || !classification.positive_gates.emotion_or_meme
        || !classification.positive_gates.reusable
        || !classification.positive_gates.context_independent
        || !classification.positive_gates.persona_fit
        || !classification.positive_gates.meaning_clear
        || !classification.positive_gates.visual_quality
    {
        bail!("accepted meme classification did not pass every positive gate")
    }
    if classification.risk_gates.ordinary_photo
        || classification.risk_gates.informational_content
        || classification.risk_gates.privacy
        || classification.risk_gates.advertisement
        || classification.risk_gates.unsafe_or_abusive
    {
        bail!("accepted meme classification triggered a risk gate")
    }
    validate_text_field("name.zh", &classification.name.zh, 1, MAX_NAME_CHARS)?;
    validate_text_field("name.en", &classification.name.en, 0, MAX_NAME_CHARS)?;
    validate_text_field(
        "description",
        &classification.description,
        1,
        MAX_DESCRIPTION_CHARS,
    )?;
    validate_text_field("usage", &classification.usage, 1, MAX_USAGE_CHARS)?;
    validate_text_field("avoid", &classification.avoid, 0, MAX_AVOID_CHARS)?;
    validate_tags(&classification.tags, true)?;
    Ok(())
}

pub(crate) fn validate_tags(tags: &[String], required: bool) -> Result<()> {
    if (required && tags.is_empty()) || tags.len() > MAX_TAGS {
        bail!(
            "tags must contain between {} and {MAX_TAGS} items",
            usize::from(required)
        )
    }
    let mut normalized = std::collections::HashSet::new();
    for tag in tags {
        validate_text_field("tag", tag, 1, MAX_TAG_CHARS)?;
        if tag.chars().any(char::is_whitespace) {
            bail!("tags must be short single tokens")
        }
        if !normalized.insert(tag.to_lowercase()) {
            bail!("tags must be unique")
        }
    }
    Ok(())
}

pub(crate) fn validate_text_field(name: &str, value: &str, min: usize, max: usize) -> Result<()> {
    let trimmed = value.trim();
    let count = trimmed.chars().count();
    if trimmed != value || count < min || count > max || value.chars().any(char::is_control) {
        bail!("{name} must be trimmed, control-free, and contain {min}..={max} characters")
    }
    Ok(())
}

pub(crate) fn validate_image_bytes(bytes: &[u8]) -> Result<ValidatedImageFormat> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("detecting image format")?;
    let image_format = reader.format().context("unsupported image format")?;
    let format = match image_format {
        image::ImageFormat::Jpeg => ValidatedImageFormat::Jpeg,
        image::ImageFormat::Png => ValidatedImageFormat::Png,
        image::ImageFormat::Gif => ValidatedImageFormat::Gif,
        image::ImageFormat::WebP => ValidatedImageFormat::Webp,
        _ => bail!("unsupported image format; supported: jpeg, png, gif, webp"),
    };
    let (width, height) = reader
        .into_dimensions()
        .context("decoding image dimensions")?;
    validate_dimensions(width, height)?;
    if format == ValidatedImageFormat::Gif {
        validate_gif(bytes)?;
    } else {
        image::load_from_memory_with_format(bytes, image_format).context("decoding image")?;
    }
    Ok(format)
}

pub(crate) fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    if !(MIN_IMAGE_EDGE..=MAX_IMAGE_EDGE).contains(&width)
        || !(MIN_IMAGE_EDGE..=MAX_IMAGE_EDGE).contains(&height)
        || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
    {
        bail!(
            "image dimensions must be {MIN_IMAGE_EDGE}..={MAX_IMAGE_EDGE} per edge and at most {MAX_IMAGE_PIXELS} pixels"
        )
    }
    Ok(())
}

fn validate_gif(bytes: &[u8]) -> Result<()> {
    let decoder = image::codecs::gif::GifDecoder::new(BufReader::new(Cursor::new(bytes)))
        .context("decoding GIF")?;
    let frames = decoder.into_frames();
    let mut frame_count = 0_usize;
    let mut duration_ms = 0_u64;
    for frame in frames {
        let frame = frame.context("decoding GIF frame")?;
        frame_count += 1;
        if frame_count > MAX_GIF_FRAMES {
            bail!("GIF must contain 1..={MAX_GIF_FRAMES} frames")
        }
        validate_dimensions(frame.buffer().width(), frame.buffer().height())?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        if denominator == 0 {
            bail!("GIF frame has an invalid delay")
        }
        duration_ms = duration_ms.saturating_add(
            u64::from(numerator).saturating_add(u64::from(denominator) - 1)
                / u64::from(denominator),
        );
        if duration_ms > MAX_GIF_DURATION_MS {
            bail!("GIF duration exceeds 15 seconds")
        }
    }
    if frame_count == 0 {
        bail!("GIF must contain 1..={MAX_GIF_FRAMES} frames")
    }
    Ok(())
}

pub(crate) async fn static_gif_preview(path: &Path) -> Result<tempfile::NamedTempFile> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&path)
            .with_context(|| format!("opening GIF {}", path.display()))?;
        let decoder = image::codecs::gif::GifDecoder::new(BufReader::new(file))
            .context("decoding GIF preview")?;
        let frame = decoder
            .into_frames()
            .next()
            .transpose()
            .context("decoding first GIF frame")?
            .context("GIF has no frames")?;
        let temp = tempfile::Builder::new().suffix(".png").tempfile()?;
        frame
            .buffer()
            .save_with_format(temp.path(), image::ImageFormat::Png)
            .context("writing static GIF preview")?;
        Ok(temp)
    })
    .await
    .context("GIF preview task failed")?
}
