#![allow(clippy::module_inception)]
use anyhow::{anyhow, bail, Context, Result};
use cosmic_text::{
    Align as TextAlign, Attrs, Buffer, Color, Family, FontSystem, LayoutGlyph, Metrics, Shaping,
    Style as FontStyle, SwashCache, Weight, Wrap,
};

use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, Pixel as _, Rgba, RgbaImage};
use pulldown_cmark::Alignment;

use std::io::{self, Write};

use unicode_segmentation::UnicodeSegmentation;

mod renderer;
pub(crate) use renderer::*;
mod handler;
pub(crate) use handler::*;
#[cfg(test)]
mod tests;
