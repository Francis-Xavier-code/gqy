use crate::i18n::text as t;
use crate::llm::{ChatResult, ChatStreamChunk, ChatStreamKind, Usage};
use crate::render::wait_spinner::{braille_frame, SpinnerStyle, WaitSpinner, SPINNER_INTERVAL};
use crate::tools::CommandOutputStream;
use anyhow::Result;
use crossterm::cursor::{Hide, MoveToColumn, MoveUp, Show};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{execute, terminal};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, IsTerminal, Write};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

mod core;
pub use core::*;
pub(crate) mod math;
mod render_impl;
pub(crate) mod wait_spinner;
pub use render_impl::*;
mod widgets;
pub use widgets::*;
mod tools;
pub use tools::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
impl StreamRenderer {}
