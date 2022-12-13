// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use std::path::PathBuf;

/// database daemon; part of Farcaster Node
#[derive(Parser, Clone, PartialEq, Eq, Debug)]
#[clap(name = "databased", bin_name = "databased", author, version)]
pub struct Opts {
    /// These params can be read also from the configuration file, not just
    /// command-line args or environment variables
    #[clap(flatten)]
    pub shared: crate::opts::Opts,
}

impl Opts {
    pub fn process(&mut self) {
        self.shared.process();
    }

    pub fn absolute_data_dir_path(&self) -> PathBuf {
        PathBuf::from(shellexpand::tilde(&self.shared.data_dir.to_string_lossy()).to_string())
    }
}
