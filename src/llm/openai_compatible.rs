mod core;
use core::*;
mod client_impl;
use client_impl::*;
mod client_impl2;
use client_impl2::*;
mod providers;
use providers::*;
mod providers2;
use providers2::*;
mod api;
use api::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
#[cfg(test)]
mod tests3;
impl OpenAiCompatibleClient {}
