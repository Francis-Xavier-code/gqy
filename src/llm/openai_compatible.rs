
mod core;
use core::*;
mod client_impl;
use client_impl::*;
mod providers;
use providers::*;
mod api;
use api::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
#[cfg(test)]
mod tests3;
impl OpenAiCompatibleClient {
}

