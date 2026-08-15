use crate::platforms::{ConversationKind, PlatformMention};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

mod store;
pub use store::*;
mod queries;
pub use queries::*;
#[cfg(test)]
mod tests;
