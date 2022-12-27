// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use crate::{AccordantBlockchain, ArbitratingBlockchain, Error};
use farcaster_core::blockchain::Network;
use internet2::addr::InetSocketAddr;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub const FARCASTER_MAINNET_ELECTRUM_SERVER: &str = "ssl://blockstream.info:700";
pub const FARCASTER_MAINNET_MONERO_DAEMON: &str = "http://node.community.rino.io:18081";
pub const FARCASTER_MAINNET_MONERO_RPC_WALLET: &str = "http://localhost:18083";

pub const FARCASTER_TESTNET_ELECTRUM_SERVER: &str = "ssl://blockstream.info:993";
pub const FARCASTER_TESTNET_MONERO_DAEMON: &str = "http://stagenet.community.rino.io:38081";
pub const FARCASTER_TESTNET_MONERO_RPC_WALLET: &str = "http://localhost:38083";

pub const FARCASTER_BIND_PORT: u16 = 7067;
pub const FARCASTER_BIND_IP: &str = "0.0.0.0";

pub const GRPC_BIND_IP_ADDRESS: &str = "127.0.0.1";

pub const SWAP_MAINNET_BITCOIN_SAFETY: u8 = 7;
pub const SWAP_MAINNET_BITCOIN_FINALITY: u8 = 6;
pub const SWAP_MAINNET_MONERO_FINALITY: u8 = 20;

pub const SWAP_TESTNET_BITCOIN_SAFETY: u8 = 3;
pub const SWAP_TESTNET_BITCOIN_FINALITY: u8 = 1;
pub const SWAP_TESTNET_MONERO_FINALITY: u8 = 1;

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct Config {
    /// Farcasterd configuration
    pub farcasterd: Option<FarcasterdConfig>,
    /// Swap configuration, applies to all swaps launched by this node
    pub swap: Option<SwapConfig>,
    /// Sets the grpc server port, if none is given, no grpc server is run
    pub grpc: Option<GrpcConfig>,
    /// Syncer configuration
    pub syncers: Option<Networked<Option<SyncerServers>>>,
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
        Ok(InetSocketAddr::from_str(&addr)?)
    }

    /// Returns a syncer configuration, if found in config, for the specified network
    pub fn get_syncer_servers(&self, network: Network) -> Option<SyncerServers> {
        match network {
            Network::Mainnet => self.syncers.as_ref()?.mainnet.clone(),
            Network::Testnet => self.syncers.as_ref()?.testnet.clone(),
            Network::Local => self.syncers.as_ref()?.local.clone(),
        }
    }

    /// Returns the addr of the tor control socket if it is set, or the default
    /// socket at localhost:9051 if not.
    pub fn get_tor_control_socket(&self) -> Result<InetSocketAddr, Error> {
        if let Some(FarcasterdConfig {
            tor_control_socket: Some(addr),
            ..
        }) = &self.farcasterd
        {
            Ok(InetSocketAddr::from_str(addr)?)
        } else {
            Ok(InetSocketAddr::from_str("127.0.0.1:9051")?)
        }
    }

    pub fn create_hidden_service(&self) -> bool {
        if let Some(FarcasterdConfig {
            create_hidden_service: Some(create_hidden_service),
            ..
        }) = &self.farcasterd
        {
            *create_hidden_service
        } else {
            false
        }
    }

    /// Returns the swap config for the specified network and arbitrating/accordant blockchains
    pub fn get_swap_config(
        &self,
        arb: ArbitratingBlockchain,
        acc: AccordantBlockchain,
        network: Network,
    ) -> Result<ParsedSwapConfig, config::ConfigError> {
        match &self.swap {
            Some(swap) => {
                let arbitrating = match arb {
                    ArbitratingBlockchain::Bitcoin => swap
                        .bitcoin
                        .get_for_network(network)
                        .or_else(|| ArbConfig::get(arb, network))
                        .ok_or_else(|| {
                            config::ConfigError::Message(
                                "No configuration nor defaults founds!".to_string(),
                            )
                        })?,
                };
                let accordant = match acc {
                    AccordantBlockchain::Monero => swap
                        .monero
                        .get_for_network(network)
                        .or_else(|| AccConfig::get(acc, network))
                        .ok_or_else(|| {
                            config::ConfigError::Message(
                                "No configuration nor defaults founds!".to_string(),
                            )
                        })?,
                };
                Ok(ParsedSwapConfig {
                    arbitrating,
                    accordant,
                })
            }
            None => {
                let arbitrating = ArbConfig::get(arb, network).ok_or_else(|| {
                    config::ConfigError::Message("No defaults founds!".to_string())
                })?;
                let accordant = AccConfig::get(acc, network).ok_or_else(|| {
                    config::ConfigError::Message("No defaults founds!".to_string())
                })?;
                Ok(ParsedSwapConfig {
                    arbitrating,
                    accordant,
                })
            }
        }
    }
}

// Default implementation is used to generate the config file on disk if not found
impl Default for Config {
    fn default() -> Self {
        Config {
            farcasterd: Some(FarcasterdConfig::default()),
            swap: Some(SwapConfig::default()),
            grpc: None,
            syncers: Some(Networked {
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
            }),
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
    /// Tor control socket for creating the hidden service
    pub tor_control_socket: Option<String>,
    /// Whether to create a hidden service or not. If set, the node will only
    /// run in hidden service mode
    pub create_hidden_service: Option<bool>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct SwapConfig {
    /// Swap parameters for the Bitcoin blockchain per network
    pub bitcoin: Networked<Option<ArbConfig>>,
    /// Swap parameters for the Monero blockchain per network
    pub monero: Networked<Option<AccConfig>>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct ParsedSwapConfig {
    /// Swap parameters for an arbitrating blockchain
    pub arbitrating: ArbConfig,
    /// Swap parameters for an accordant blockchain
    pub accordant: AccConfig,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct ArbConfig {
    /// Avoid broadcasting a transaction if a race can happen with the next available execution
    /// fork in # blocks
    pub safety: u8,
    /// Number of confirmations required to consider a transaction final
    pub finality: u8,
}

impl ArbConfig {
    fn get(blockchain: ArbitratingBlockchain, network: Network) -> Option<Self> {
        match blockchain {
            ArbitratingBlockchain::Bitcoin => match network {
                Network::Mainnet => Some(Self::btc_mainnet_default()),
                Network::Testnet => Some(Self::btc_testnet_default()),
                Network::Local => None,
            },
        }
    }

    fn btc_mainnet_default() -> Self {
        ArbConfig {
            safety: SWAP_MAINNET_BITCOIN_SAFETY,
            finality: SWAP_MAINNET_BITCOIN_FINALITY,
        }
    }

    fn btc_testnet_default() -> Self {
        ArbConfig {
            safety: SWAP_TESTNET_BITCOIN_SAFETY,
            finality: SWAP_TESTNET_BITCOIN_FINALITY,
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct AccConfig {
    /// Number of confirmations required to consider a transaction final
    pub finality: u8,
}

impl AccConfig {
    fn get(blockchain: AccordantBlockchain, network: Network) -> Option<Self> {
        match blockchain {
            AccordantBlockchain::Monero => match network {
                Network::Mainnet => Some(Self::xmr_mainnet_default()),
                Network::Testnet => Some(Self::xmr_testnet_default()),
                Network::Local => None,
            },
        }
    }

    fn xmr_mainnet_default() -> Self {
        AccConfig {
            finality: SWAP_MAINNET_MONERO_FINALITY,
        }
    }

    fn xmr_testnet_default() -> Self {
        AccConfig {
            finality: SWAP_TESTNET_MONERO_FINALITY,
        }
    }
}

// Arbitrating blockchains are a superset of accordant ones
impl From<ArbConfig> for AccConfig {
    fn from(arb: ArbConfig) -> Self {
        Self {
            finality: arb.finality,
        }
    }
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

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct Networked<T> {
    /// A configuration section applied to the mainnet network
    pub mainnet: T,
    /// A configuration section applied to the live testnet network
    pub testnet: T,
    /// A configuration section applied to the local network (regtest)
    pub local: T,
}

impl<T> Networked<T>
where
    T: Clone,
{
    fn get_for_network(&self, network: Network) -> T {
        match network {
            Network::Mainnet => self.mainnet.clone(),
            Network::Testnet => self.testnet.clone(),
            Network::Local => self.local.clone(),
        }
    }
}

// Default implementation is used to generate the config file on disk if not found
impl Default for SwapConfig {
    fn default() -> Self {
        SwapConfig {
            bitcoin: Networked {
                mainnet: Some(ArbConfig::btc_mainnet_default()),
                testnet: Some(ArbConfig::btc_testnet_default()),
                local: None,
            },
            monero: Networked {
                mainnet: Some(AccConfig::xmr_mainnet_default()),
                testnet: Some(AccConfig::xmr_testnet_default()),
                local: None,
            },
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
            tor_control_socket: None,
            create_hidden_service: None,
        }
    }
}

/// Parse a configuration file and return a [`Config`]
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

#[cfg(test)]
mod tests {
    use super::parse_config;

    #[test]
    fn config_example_parse() {
        let config = parse_config("./farcasterd.toml").expect("correct config example");
        dbg!(config);
    }
}
