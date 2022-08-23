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
