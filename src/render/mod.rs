use crate::i18n::text as t;
use crate::llm::{ChatStreamChunk, ChatStreamKind};
use crate::render::wait_spinner::{SpinnerStyle, WaitSpinner, SPINNER_INTERVAL};
use crate::tools::CommandOutputStream;
use anyhow::Result;
use crossterm::cursor::{Hide, MoveToColumn, Show};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{execute, terminal};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};

mod core;
pub(crate) use core::*;
pub(crate) mod math;
mod render_impl;
pub(crate) mod wait_spinner;

mod widgets;
pub(crate) use widgets::*;
mod tools;
pub(crate) use tools::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
impl StreamRenderer {}
