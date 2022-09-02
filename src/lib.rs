// LNP Node: node running lightning network protocol and generalized lightning
// channels.
// Written in 2020 by
//     Dr. Maxim Orlovsky <orlovsky@pandoracore.com>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the MIT License
// along with this software.
// If not, see <https://opensource.org/licenses/MIT>.
#![recursion_limit = "256"]
// Coding conventions
#![deny(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    unused_mut,
    unused_imports,
    dead_code
)]

#[macro_use]
extern crate amplify;
#[macro_use]
extern crate amplify_derive;

#[cfg(feature = "shell")]
#[macro_use]
extern crate clap;
#[macro_use]
extern crate log;

#[cfg(feature = "serde")]
// on the `[dependencies]` section of `Cargo.toml`, check the comments above
// the `serde_crate` dependency; we can remove this rename once we move to
// version 1.60.0 of Rust
extern crate serde_crate as serde;
#[cfg(feature = "serde")]
#[macro_use]
extern crate serde_with;

#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "_rpc")]
pub mod config;
pub mod error;
pub mod event;
#[cfg(feature = "shell")]
pub mod opts;
#[cfg(feature = "_rpc")]
pub mod rpc;

#[cfg(feature = "node")]
pub mod databased;
#[cfg(feature = "node")]
pub mod farcasterd;
#[cfg(feature = "node")]
pub mod grpcd;
#[cfg(feature = "node")]
pub mod peerd;
#[cfg(feature = "_rpc")]
mod service;
#[cfg(feature = "node")]
pub mod swapd;
#[cfg(feature = "node")]
pub mod syncerd;
#[cfg(feature = "node")]
pub mod walletd;

#[cfg(feature = "_rpc")]
pub use crate::config::Config;
#[cfg(feature = "_rpc")]
pub use crate::service::ServiceConfig;
pub use error::Error;
#[cfg(feature = "_rpc")]
pub use service::{CtlServer, Endpoints, LogStyle, Service, ServiceId, TryToServiceId};
