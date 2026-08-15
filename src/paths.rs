use crate::i18n::text as t;
use anyhow::{bail, Context, Result};
use directories::{BaseDirs, UserDirs};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{symlink, DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

mod layout;
pub use layout::*;
mod migration;
pub use migration::*;
#[cfg(test)]
mod tests;
