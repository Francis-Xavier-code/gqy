use super::{vision, ToolProgress, ToolRegistry, ToolSpec};
use crate::config::{AppConfig, ProviderConfig, VisionPluginConfig};
use crate::llm::{ChatMessage, OpenAiCompatibleClient};
use crate::paths::GQYPaths;
use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb, RgbImage};
use reqwest::{Client, Url};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::net::IpAddr;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod images;
pub(crate) use images::*;
mod fetch;
pub(crate) use fetch::*;
#[cfg(test)]
mod tests;
