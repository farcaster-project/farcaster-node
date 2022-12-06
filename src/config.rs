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
use internet2::addr::InetSocketAddr;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub const FARCASTER_MAINNET_ELECTRUM_SERVER: &str = "ssl://blockstream.info:700";
pub const FARCASTER_MAINNET_MONERO_DAEMON: &str = "http://node.monerooutreach.org:18081";
pub const FARCASTER_MAINNET_MONERO_RPC_WALLET: &str = "http://localhost:18083";

pub const FARCASTER_TESTNET_ELECTRUM_SERVER: &str = "ssl://blockstream.info:993";
pub const FARCASTER_TESTNET_MONERO_DAEMON: &str = "http://stagenet.community.rino.io:38081";
pub const FARCASTER_TESTNET_MONERO_RPC_WALLET: &str = "http://localhost:38083";

pub const FARCASTER_BIND_PORT: u16 = 7067;
pub const FARCASTER_BIND_IP: &str = "0.0.0.0";

pub const GRPC_BIND_IP_ADDRESS: &str = "127.0.0.1";

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct Config {
    /// Farcasterd configuration
    pub farcasterd: Option<FarcasterdConfig>,
    /// Sets the grpc server port, if none is given, no grpc server is run
    pub grpc: Option<GrpcConfig>,
    /// Syncer configuration
    pub syncers: Option<SyncersConfig>,
}

impl Config {
    /// Returns if auto-funding functionality is enabled
    pub fn is_auto_funding_enable(&self) -> bool {
        match &self.farcasterd {
            Some(FarcasterdConfig {
                auto_funding: Some(AutoFundingConfig { enable, .. }),
                ..
            }) => *enable,
            _ => false,
        }
    }

    /// Returns if grpc is enabled
    pub fn is_grpc_enable(&self) -> bool {
        match &self.grpc {
            Some(GrpcConfig { enable, .. }) => *enable,
            _ => false,
        }
    }

    /// Returns the Grcp bind ip address, if not set return the default value
    pub fn grpc_bind_ip(&self) -> String {
        match &self.grpc {
            Some(GrpcConfig {
                bind_ip: Some(bind_ip),
                ..
            }) => bind_ip.clone(),
            _ => String::from(GRPC_BIND_IP_ADDRESS),
        }
    }

    /// Returns if auto restore is enabled. Default to true
    pub fn auto_restore_enable(&self) -> bool {
        match &self.farcasterd {
            Some(FarcasterdConfig {
                auto_restore: Some(enable),
                ..
            }) => *enable,
            _ => true,
        }
    }

    /// Returns the auto-funding configuration for a given network if enable, if None no
    /// configuration is found
    pub fn get_auto_funding_config(&self, network: Network) -> Option<AutoFundingServers> {
        match &self.farcasterd {
            Some(FarcasterdConfig {
                auto_funding:
                    Some(AutoFundingConfig {
                        enable,
                        mainnet,
                        testnet,
                        local,
                    }),
                ..
            }) if *enable => match network {
                Network::Mainnet => mainnet.clone(),
                Network::Testnet => testnet.clone(),
                Network::Local => local.clone(),
            },
            _ => None,
        }
    }

    /// Returns the bind address to use to instanciate listening peerd, either loads the values
    /// from the config file or use the default values '0.0.0.0:7067'
    pub fn get_bind_addr(&self) -> Result<InetSocketAddr, Error> {
        let addr = if let Some(FarcasterdConfig {
            bind_ip, bind_port, ..
        }) = &self.farcasterd
        {
            format!(
                "{}:{}",
                bind_ip.as_ref().unwrap_or(&FARCASTER_BIND_IP.to_string()),
                bind_port.unwrap_or(FARCASTER_BIND_PORT)
            )
        } else {
            format!("{}:{}", FARCASTER_BIND_IP, FARCASTER_BIND_PORT)
        };
        Ok(InetSocketAddr::from_str(&addr).map_err(|e| Error::Farcaster(e.to_string()))?)
    }

    pub fn get_syncer_servers(&self, network: Network) -> Option<SyncerServers> {
        match network {
            Network::Mainnet => self.syncers.as_ref()?.mainnet.clone(),
            Network::Testnet => self.syncers.as_ref()?.testnet.clone(),
            Network::Local => self.syncers.as_ref()?.local.clone(),
        }
    }
}

// Default implementation is used to generate the config file on disk if not found
impl Default for Config {
    fn default() -> Self {
        Config {
            farcasterd: Some(FarcasterdConfig::default()),
            grpc: None,
            syncers: Some(SyncersConfig::default()),
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct FarcasterdConfig {
    /// Sets the auto-funding parameters, default to no auto-fund
    pub auto_funding: Option<AutoFundingConfig>,
    /// Sets the bind port for potential makers
    pub bind_port: Option<u16>,
    /// Sets the bind ip for potential makers
    pub bind_ip: Option<String>,
    /// Whether checkpoints should be auto restored at start-up, or not
    pub auto_restore: Option<bool>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct GrpcConfig {
    /// Use grpc functionality
    pub enable: bool,
    /// Grpc port configuration
    pub bind_port: u16,
    /// Grpc listening ip address
    pub bind_ip: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct AutoFundingConfig {
    /// Use auto-funding functionality
    pub enable: bool,
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
    pub bitcoin_cookie_path: Option<String>,
    /// RPC user to connect to the bitcoin-core node
    pub bitcoin_rpc_user: Option<String>,
    /// RPC pass to connect to the bitcoin-core node
    pub bitcoin_rpc_pass: Option<String>,
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

// Default implementation is used to generate the config file on disk if not found
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

// Default implementation is used to generate the config file on disk if not found
impl Default for FarcasterdConfig {
    fn default() -> Self {
        FarcasterdConfig {
            auto_funding: None,
            // write the default config for auto-restore
            auto_restore: Some(true),
            // write the default port and ip in the generated config
            bind_port: Some(FARCASTER_BIND_PORT),
            bind_ip: Some(FARCASTER_BIND_IP.to_string()),
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
