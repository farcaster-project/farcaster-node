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

use clap::{AppSettings, ArgGroup, Clap, ValueHint};
use std::net::IpAddr;
use std::path::PathBuf;
use std::{fs, io::Read};

use internet2::{FramingProtocol, LocalNode, RemoteNodeAddr};
use lnpbp::strict_encoding::{StrictDecode, StrictEncode};

use crate::opts::FARCASTER_NODE_KEY_FILE;

/// Lightning peer network connection daemon; part of LNP Node
///
/// Daemon listens to incoming connections from the lightning network peers
/// (if started with `--listen` argument) or connects to the remote peer
/// (specified with `--connect` argument) and passes all incoming messages into
/// ZMQ messaging socket (controlled with `--msg-socket` argument, defaulting to
/// `msg.rpc` file inside the data directory from `--data-dir`). It also
/// forwards messages from the same socket to the remote peer.
///
/// The daemon is controlled though ZMQ ctl socket (see `ctl-socket` argument
/// description)
#[derive(Clap, Clone, PartialEq, Eq, Debug)]
#[clap(
    name = "peerd",
    bin_name = "peerd",
    author,
    version,
    group = ArgGroup::new("action").required(true),
    setting = AppSettings::ColoredHelp
)]
pub struct Opts {
    // These params are passed through command-line argument or environment
    // only since they are instance-specific
    /// Start daemon in listening mode binding the provided local address
    ///
    /// Binds to the specified interface and listens for incoming connections,
    /// spawning a new thread / forking child process for each new incoming
    /// client connecting the opened socket. Whether the child is spawned as a
    /// thread or forked as a child process determined by the presence of
    /// `--use-threads` flag.
    /// If the argument is provided in form of flag, without value, uses
    /// `0.0.0.0` as the bind address.
    #[clap(short = 'L', long, group = "action", value_hint = ValueHint::Hostname)]
    pub listen: Option<Option<IpAddr>>,

    /// Connect to a remote peer with the provided address after start
    ///
    /// Connects to the specified remote peer. Peer address should be given as
    /// either IPv4, IPv6 or Onion address (v2 or v3); in the former case you
    /// will be also required to provide `--tor` argument.
    #[clap(short = 'C', long, group = "action")]
    pub connect: Option<RemoteNodeAddr>,

    /// Customize port used by lightning peer network
    ///
    /// Optional argument specifying local or remote TCP port to use with the
    /// address given to `--listen` or `--connect` argument.
    #[clap(short, long, default_value = "9735")]
    pub port: u16,

    /// Overlay peer communications through different transport protocol.
    #[clap(
        short,
        long,
        default_value = "tcp",
        possible_values = &["tcp", "zmq", "http", "websocket", "smtp"]
    )]
    pub overlay: FramingProtocol,

    /// Node key configuration
    #[clap(flatten)]
    pub key_opts: KeyOpts,

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

use bitcoin::secp256k1::{
    rand::{rngs::ThreadRng, thread_rng},
    SecretKey,
};
use lnpbp::strict_encoding;

/// Hold secret keys and seeds
#[derive(StrictEncode, StrictDecode)]
pub struct NodeSecrets {
    /// local node private information
    local_node: LocalNode,
    /// seed used for deriving addresses
    wallet_seed: [u8; 32],
}

impl KeyOpts {
    pub fn process(&mut self, shared: &crate::opts::Opts) {
        shared.process_dir(&mut self.key_file);
    }

    pub fn local_node(&self) -> LocalNode {
        self.node_secrets().local_node
    }

    pub fn wallet_seed(&self) -> [u8; 32] {
        self.node_secrets().wallet_seed
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
            // validate that we can convert the seed back into the secret key
            let expected_key = SecretKey::from_slice(&seed_buf).expect("wallet_seed has 32 bytes");
            assert_eq!(
                expected_key, key,
                "Cannot go back and forward from bytes to secret key"
            );
        }
        seed_buf
    }

    pub fn node_secrets(&self) -> NodeSecrets {
        if PathBuf::from(self.key_file.clone()).exists() {
            NodeSecrets::strict_decode(fs::File::open(&self.key_file).expect(&format!(
                "Unable to open key file {}; please check that the user \
                     running the daemon has necessary permissions",
                self.key_file
            )))
            .expect("Unable to read node code file format")
        } else {
            let mut rng = thread_rng();
            let private_key = SecretKey::new(&mut rng);
            let ephemeral_private_key = SecretKey::new(&mut rng);
            let local_node = LocalNode::from_keys(private_key, ephemeral_private_key);
            let wallet_seed = Self::create_seed(&mut rng);
            let node_secrets = NodeSecrets {
                local_node,
                wallet_seed,
            };

            let key_file = fs::File::create(&self.key_file).expect(&format!(
                "Unable to create key file '{}'; please check that the path exists",
                self.key_file
            ));
            node_secrets
                .strict_encode(key_file)
                .expect("Unable to save generated node secrets");
            node_secrets
        }
    }
}
