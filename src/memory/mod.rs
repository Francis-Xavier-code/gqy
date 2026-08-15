use crate::config::{AppConfig, KnowledgeBasePluginConfig, MemoryConfig};
use crate::paths::GQYPaths;
use crate::platforms::PlatformPrincipal;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::LazyLock;

mod core;
use core::*;
mod store_impl;
use store_impl::*;
mod ops;
use ops::*;
mod organizer;
#[cfg(test)]
mod tests;

pub(crate) use organizer::{MemoryOrganizer, MemoryOrganizerHandle};

const SHORT_TERM: &str = "short_term";
const LONG_TERM: &str = "long_term";
const VISIBILITY_PUBLIC: &str = "public";
const VISIBILITY_PRINCIPAL: &str = "principal";
const VISIBILITY_PRIVILEGED: &str = "privileged";
const MAX_ORGANIZED_ITEMS: usize = 20;
const JIEBA_INDEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/jieba.fst"));
static JIEBA: LazyLock<CompactJieba> = LazyLock::new(|| {
    CompactJieba::new().expect("the build-generated compact Jieba index must be valid")
});

struct CompactJieba {
    words: fst::Map<&'static [u8]>,
    log_total: f64,
    max_word_chars: usize,
}

impl MemoryStore {}
