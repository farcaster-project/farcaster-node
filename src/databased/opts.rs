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

use crate::opts::FARCASTER_KEY_FILE;
use clap::{AppSettings, Parser, ValueHint};
use std::path::PathBuf;
use std::{fs, io::Read};

use crate::opts::TokenString;
use bitcoin::secp256k1::{
    rand::{rngs::ThreadRng, thread_rng},
    PublicKey, Secp256k1, SecretKey,
};
use strict_encoding::{StrictDecode, StrictEncode};

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
