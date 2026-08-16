use crate::agent::{
    archive_and_delete_visible_turns, Agent, AgentEvent, AgentMode, AgentTurnControl,
};
use crate::config::{ActiveProviderModelConfig, AppConfig};
use crate::i18n::{is_zh, text as t};
use crate::ipc::{self, Command as IpcCommand, Frame as IpcFrame, Request as IpcRequest};
use crate::llm::{
    ChatResult, ChatStreamChunk, OpenAiCompatibleClient, ThinkingVariantOptions, TurnTokens, Usage,
};
use crate::memory::{MemoryOrganizer, MemoryStore};
use crate::paths::GQYPaths;
use crate::render;
use crate::shell;
use crate::state::{QueuedPrompt, QueuedPromptAttachment, StateStore, Turn, TurnStatus};
use crate::tools;
use anyhow::{bail, Context, Result};
use base64::Engine;
use chrono::{DateTime, Local};
use clap::{Arg, ArgAction, Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use crossterm::cursor::{self, Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use crossterm::style::{Color, Print, Stylize};
use crossterm::terminal::{self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate};
use crossterm::{execute, queue};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::ffi::OsString;
use std::io::Cursor;
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use vte::{Params as VteParams, Parser as VteParser, Perform as VtePerform};

mod defs;
pub(crate) use defs::*;
mod daemon;
pub(crate) use daemon::*;
mod init;
pub(crate) use init::*;
mod fuzzy;
pub(crate) use fuzzy::*;
mod oneshot;
pub(crate) use oneshot::*;
mod ipc_impl;
pub(crate) use ipc_impl::*;
mod remote;
pub(crate) use remote::*;
mod direct;
pub(crate) use direct::*;
mod variant;
pub(crate) use variant::*;
mod live;
pub(crate) use live::*;
mod live2;
pub(crate) use live2::*;
mod jobs;
pub(crate) use jobs::*;
mod repl_ui;
pub(crate) use repl_ui::*;
mod keyboard_enhancement;
use keyboard_enhancement::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
#[cfg(test)]
mod tests3;
