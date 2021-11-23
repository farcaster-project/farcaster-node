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

use crate::syncerd::runtime::SyncerServers;
use crate::Error;
use farcaster_core::blockchain::Network;
use internet2::NodeAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const FARCASTER_MAINNET_ELECTRUM_SERVER: &str = "ssl://blockstream.info:700";
pub const FARCASTER_MAINNET_MONERO_DAEMON: &str = "http://node.monerooutreach.org:18081";
pub const FARCASTER_MAINNET_MONERO_RPC_WALLET: &str = "http://localhost:18083";

pub const FARCASTER_TESTNET_ELECTRUM_SERVER: &str = "ssl://blockstream.info:993";
pub const FARCASTER_TESTNET_MONERO_DAEMON: &str = "http://stagenet.melo.tools:38081";
pub const FARCASTER_TESTNET_MONERO_RPC_WALLET: &str = "http://localhost:38083";

#[cfg(feature = "shell")]
use crate::opts::Opts;

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct Config {
    /// Syncer configuration
    pub syncers: Option<Syncers>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            syncers: Some(Syncers::default()),
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct Syncers {
    /// Mainnet syncer configuration
    pub mainnet: Option<SyncerServers>,

    /// Testnet syncer configuration
    pub testnet: Option<SyncerServers>,
}

impl Default for Syncers {
    fn default() -> Self {
        Syncers {
            mainnet: Some(SyncerServers {
                electrum_server: FARCASTER_MAINNET_ELECTRUM_SERVER.into(),
                monero_daemon: FARCASTER_MAINNET_ELECTRUM_SERVER.into(),
                monero_rpc_wallet: FARCASTER_MAINNET_ELECTRUM_SERVER.into(),
            }),
            testnet: Some(SyncerServers {
                electrum_server: FARCASTER_TESTNET_ELECTRUM_SERVER.into(),
                monero_daemon: FARCASTER_TESTNET_ELECTRUM_SERVER.into(),
                monero_rpc_wallet: FARCASTER_TESTNET_ELECTRUM_SERVER.into(),
            }),
        }
    }
}

pub fn parse_config(path: &str) -> Result<Config, config::ConfigError> {
    if Path::new(path).exists() {
        let config_file = path;
        debug!("Loading config file at: {}", &config_file);
        let mut settings = config::Config::default();
        settings.merge(config::File::with_name(config_file).required(true))?;
        settings.try_into::<Config>()
    } else {
        debug!("No configuration file found, generate default config");
        Ok(Config::default())
    }
}
