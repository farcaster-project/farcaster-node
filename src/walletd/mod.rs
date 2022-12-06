// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

#[cfg(feature = "shell")]
mod opts;
pub mod runtime;

#[cfg(feature = "shell")]
pub use opts::{KeyOpts, NodeSecrets, Opts};
pub use runtime::run;
