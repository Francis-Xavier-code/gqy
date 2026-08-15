use super::{vision, ToolProgress, ToolRegistry, ToolSpec};
use crate::config::{AppConfig, ProviderConfig, VisionPluginConfig};
use crate::i18n::{agent_text, text as t};
use crate::llm::{ChatMessage, OpenAiCompatibleClient};
use crate::paths::GQYPaths;
use anyhow::{bail, Context, Result};
use futures_util::{future::join_all, StreamExt};
use image::{DynamicImage, GenericImageView, ImageBuffer, ImageFormat, Rgb, RgbImage};
use reqwest::{Client, Url};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::net::IpAddr;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

mod images;
pub use images::*;
mod fetch;
pub use fetch::*;
#[cfg(test)]
mod tests;
