mod registry;
use registry::*;
mod access_control;
mod adapters;
mod assets;
pub(crate) mod avatar;
pub(crate) mod commands;
pub(crate) mod onebot;
pub(crate) mod plugins;
mod tool;
mod types;
use adapters::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
