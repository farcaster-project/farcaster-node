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

use crate::Error;
use farcaster_core::blockchain::Network;
use internet2::NodeAddr;
use std::fs::File;
use std::io::prelude::*;
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
    /// Farcasterd configuration
    pub farcasterd: Option<FarcasterdConfig>,
    /// Syncer configuration
    pub syncers: Option<SyncersConfig>,
}

impl Config {
    /// Returns if auto-funding functionality is enable
    pub fn is_auto_funding_enable(&self) -> bool {
        match &self.farcasterd {
            Some(FarcasterdConfig {
                auto_funding: Some(AutoFundingConfig { auto_fund, .. }),
            }) => *auto_fund,
            _ => false,
        }
    }

    /// Returns the auto-funding configuration for a given network if enable, if None no
    /// configuration is found
    pub fn get_auto_funding_config(&self, network: Network) -> Option<AutoFundingServers> {
        match &self.farcasterd {
            Some(FarcasterdConfig {
                auto_funding:
                    Some(AutoFundingConfig {
                        auto_fund,
                        mainnet,
                        testnet,
                        local,
                    }),
            }) if *auto_fund => match network {
                Network::Mainnet => mainnet.clone(),
                Network::Testnet => testnet.clone(),
                Network::Local => local.clone(),
            },
            _ => None,
        }
    }

    pub fn get_syncer_servers(&self, network: Network) -> Option<SyncerServers> {
        match network {
            Network::Mainnet => self.syncers.as_ref()?.mainnet.clone(),
            Network::Testnet => self.syncers.as_ref()?.testnet.clone(),
            Network::Local => self.syncers.as_ref()?.local.clone(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            farcasterd: None,
            syncers: Some(SyncersConfig::default()),
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct FarcasterdConfig {
    /// Sets the auto-funding parameters, default to no auto-fund
    pub auto_funding: Option<AutoFundingConfig>,
}

impl Default for FarcasterdConfig {
    fn default() -> Self {
        FarcasterdConfig { auto_funding: None }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct AutoFundingConfig {
    /// Use auto-funding functionality
    pub auto_fund: bool,
    /// Mainnet auto-funding configuration
    pub mainnet: Option<AutoFundingServers>,
    /// Testnet auto-funding configuration
    pub testnet: Option<AutoFundingServers>,
    /// Local auto-funding configuration
    pub local: Option<AutoFundingServers>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct AutoFundingServers {
    /// The host and port with scheme for connecting to bitcoin-core node
    pub bitcoin_rpc: String,
    /// Path to the cookie file to connect to the bitcoin-core node
    pub bitcoin_cookie_path: String,
    /// The monero wallet rpc url to auto-fund monero
    pub monero_rpc_wallet: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct SyncersConfig {
    /// Mainnet syncer configuration
    pub mainnet: Option<SyncerServers>,
    /// Testnet syncer configuration
    pub testnet: Option<SyncerServers>,
    /// Local syncer configuration
    pub local: Option<SyncerServers>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct SyncerServers {
    /// Electrum server to use
    pub electrum_server: String,
    /// Monero daemon to use
    pub monero_daemon: String,
    /// Monero rpc wallet to use
    pub monero_rpc_wallet: String,
    /// Monero lws to use
    pub monero_lws: Option<String>,
    /// Monero wallet directory
    pub monero_wallet_dir: Option<String>,
}

impl Default for SyncersConfig {
    fn default() -> Self {
        SyncersConfig {
            mainnet: Some(SyncerServers {
                electrum_server: FARCASTER_MAINNET_ELECTRUM_SERVER.into(),
                monero_daemon: FARCASTER_MAINNET_MONERO_DAEMON.into(),
                monero_rpc_wallet: FARCASTER_MAINNET_MONERO_RPC_WALLET.into(),
                monero_lws: None,
                monero_wallet_dir: None,
            }),
            testnet: Some(SyncerServers {
                electrum_server: FARCASTER_TESTNET_ELECTRUM_SERVER.into(),
                monero_daemon: FARCASTER_TESTNET_MONERO_DAEMON.into(),
                monero_rpc_wallet: FARCASTER_TESTNET_MONERO_RPC_WALLET.into(),
                monero_lws: None,
                monero_wallet_dir: None,
            }),
            local: None,
        }
    }
}

pub fn parse_config(path: &str) -> Result<Config, Error> {
    if Path::new(path).exists() {
        let config_file = path;
        info!("Loading config file at: {}", &config_file);
        let mut settings = config::Config::default();
        settings.merge(config::File::with_name(config_file).required(true))?;
        settings.try_into::<Config>().map_err(Into::into)
    } else {
        info!("No configuration file found, generating default config");
        let config = Config::default();
        let mut file = File::create(path)?;
        file.write_all(toml::to_vec(&config).unwrap().as_ref())?;
        Ok(config)
    }
}
