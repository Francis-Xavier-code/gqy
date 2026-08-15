use crate::default_models::{
    OPENCODE_DEFAULT_CHAT_MODEL, OPENCODE_DEFAULT_VISION_MODEL, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_BASE_URL,
};
use crate::paths::GQYPaths;
use crate::prompts::default_system_prompt;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

mod core;
pub use core::*;
mod plugins;
pub use plugins::*;
mod defaults;
pub use defaults::*;
mod app_impl;

mod app_impl2;

mod schema;
pub use schema::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
#[cfg(test)]
mod tests3;
impl AppConfig {}
