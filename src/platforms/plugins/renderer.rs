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

mod renderer;
pub use renderer::*;
mod handler;
pub use handler::*;
#[cfg(test)]
mod tests;
