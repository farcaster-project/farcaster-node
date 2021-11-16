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

use crate::opts::FARCASTER_NODE_KEY_FILE;
use clap::{AppSettings, Clap, ValueHint};
use std::path::PathBuf;
use std::{fs, io::Read};

/// Walletd daemon; part of Farcaster Node
#[derive(Clap, Clone, PartialEq, Eq, Debug)]
#[clap(
    name = "walletd",
    bin_name = "walletd",
    author,
    version,
    setting = AppSettings::ColoredHelp
)]
pub struct Opts {
    /// Node key configuration
    #[clap(flatten)]
    pub key_opts: KeyOpts,

    /// Walletd token
    #[clap(flatten)]
    pub token: WalletToken,

    /// These params can be read also from the configuration file, not just
    /// command-line args or environment variables
    #[clap(flatten)]
    pub shared: crate::opts::Opts,
}

impl Opts {
    pub fn process(&mut self) {
        self.shared.process();
        self.key_opts.process(&self.shared);
    }
}

/// Node key configuration
#[derive(Clap, Clone, PartialEq, Eq, Debug)]
pub struct KeyOpts {
    /// Node key file
    ///
    /// Location for the file containing node private Secp256k1 key
    /// (unencrypted)
    #[clap(
        short,
        long,
        env = "FARCASTER_NODE_KEY_FILE",
        default_value = FARCASTER_NODE_KEY_FILE,
        value_hint = ValueHint::FilePath
    )]
    pub key_file: String,
}

#[derive(Clap, Clone, PartialEq, Eq, Debug)]
pub struct WalletToken {
    #[clap(short, long, env = "FARCASTER_WALLETD_TOKEN", default_value = "")]
    pub wallet_token: String,
}

use bitcoin::secp256k1::{
    rand::{rngs::ThreadRng, thread_rng},
    PublicKey, Secp256k1, SecretKey,
};
use strict_encoding;
use strict_encoding::{StrictDecode, StrictEncode};

#[derive(StrictEncode, StrictDecode, Clone, PartialEq, Eq, Debug)]
pub struct Counter(pub u32);
impl Counter {
    fn increment(&mut self) -> u32 {
        self.0 += 1;
        self.0
    }
}

/// Hold secret keys and seeds
#[derive(StrictEncode, StrictDecode, Clone, PartialEq, Eq, Debug)]
pub struct NodeSecrets {
    /// local key file
    pub key_file: String,
    /// local node private information
    pub peerd_secret_key: SecretKey,
    /// seed used for deriving addresses
    pub wallet_seed: [u8; 32],
    /// wallet last derivation index
    pub wallet_counter: Counter,
}

impl NodeSecrets {
    pub fn new(key_file: String) -> Self {
        if PathBuf::from(key_file.clone()).exists() {
            NodeSecrets::strict_decode(fs::File::open(key_file.clone()).expect(&format!(
                "Unable to open key file {}; please check that the user \
                    running the deamon has necessary permissions",
                key_file
            )))
            .expect("Unable to read node code file format")
        } else {
            let mut rng = thread_rng();
            let peer_private_key = SecretKey::new(&mut rng);
            let wallet_seed = Self::create_seed(&mut rng);
            let node_secrets = Self {
                key_file: key_file.clone(),
                peerd_secret_key: peer_private_key,
                wallet_seed,
                wallet_counter: Counter(0),
            };

            let key_file_handle = fs::File::create(&key_file).expect(&format!(
                "Unable to create key file '{}'; please check that path exists",
                key_file
            ));
            node_secrets
                .strict_encode(key_file_handle)
                .expect("Unable to save generated node secrets");
            node_secrets
        }
    }

    pub fn node_id(&self) -> PublicKey {
        PublicKey::from_secret_key(&Secp256k1::new(), &self.peerd_secret_key)
    }

    fn create_seed(rng: &mut ThreadRng) -> [u8; 32] {
        let mut seed_buf = [0u8; 32];
        let key = SecretKey::new(rng);
        let mut key_iter = key[..].iter();
        let mut reader = std::io::Cursor::new(&mut key_iter);
        reader
            .read_exact(&mut seed_buf)
            .expect("wallet_key has 32 bytes");
        {
            let expected_key = SecretKey::from_slice(&seed_buf).expect("wallet_seed has 32 bytes");
            assert_eq!(
                expected_key, key,
                "Cannot go back and forward from bytes to secret key"
            );
        }
        seed_buf
    }

    pub fn increment_wallet_counter(&mut self) -> u32 {
        self.wallet_counter.increment();
        let key_file_handle = fs::File::create(&self.key_file).expect(&format!(
            "Unable to create key file '{}'; please check that path exists",
            self.key_file
        ));
        self.strict_encode(key_file_handle)
            .expect("Unable to save incremented wallet counter");
        self.wallet_counter.0
    }

    pub fn wallet_seed(&self) -> [u8; 32] {
        self.wallet_seed
    }
}

impl KeyOpts {
    pub fn process(&mut self, shared: &crate::opts::Opts) {
        shared.process_dir(&mut self.key_file);
    }
}
