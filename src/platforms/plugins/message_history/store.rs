#![allow(clippy::module_inception)]
use crate::platforms::PlatformMention;
use anyhow::{bail, Result};

use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use serde::Deserialize;

mod store;
pub(crate) use store::*;
mod queries;
pub(crate) use queries::*;
#[cfg(test)]
mod tests;
