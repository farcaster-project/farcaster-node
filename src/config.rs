// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use farcaster_core::blockchain::Network;
use farcaster_core::swap::btcxmr::DealParameters;
use internet2::addr::InetSocketAddr;
use serde::{Deserialize, Serialize};
use serde_with::DisplayFromStr;

use std::convert::TryInto;
use std::fmt::Display;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use std::str::FromStr;

use crate::{AccordantBlockchain, ArbitratingBlockchain, Error};

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
pub const SWAP_MAINNET_BITCOIN_MAX_BTC_AMOUNT: f64 = 0.01;
pub const SWAP_MAINNET_MONERO_FINALITY: u8 = 20;
pub const SWAP_MAINNET_MONERO_MIN_XMR_AMOUNT: f64 = 0.001;
pub const SWAP_MAINNET_MONERO_MAX_XMR_AMOUNT: f64 = 2.0;

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
                        .map(|c| c.temporality)
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
                        .map(|c| c.temporality)
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

    /// Validate a deal against user configuration (farcasterd.toml) and user provided addresses
    pub fn validate_deal_parameters(
        &self,
        deal: &DealParameters,
        arb_addr: &bitcoin::Address,
        acc_addr: &monero::Address,
    ) -> Result<(), Error> {
        self.validate_deal_addresses(deal, arb_addr, acc_addr)?;
        self.validate_deal_amounts(deal)
    }

    /// Validate deal amounts against user configuration (farcasterd.toml)
    pub fn validate_deal_amounts(&self, deal: &DealParameters) -> Result<(), Error> {
        // assume arbitrating is bitcoin
        self.swap
            .as_ref()
            .and_then(|swap| {
                swap.bitcoin
                    .get_for_network(deal.network)
                    .map(|net| net.amounts)
            })
            .or_else(|| Self::btc_default_tradeable(deal.network))
            .map(|tradeable| tradeable.validate_amount(deal.arbitrating_amount))
            .transpose()?;
        // assume accordant is monero
        self.swap
            .as_ref()
            .and_then(|swap| {
                swap.monero
                    .get_for_network(deal.network)
                    .map(|net| net.amounts)
            })
            .or_else(|| Self::xmr_default_tradeable(deal.network))
            .map(|tradeable| tradeable.validate_amount(deal.accordant_amount))
            .transpose()?;

        Ok(())
    }

    /// Validate user addresses for arbitrating and accordant blockchain against a deal
    pub fn validate_deal_addresses(
        &self,
        deal: &DealParameters,
        arb_addr: &bitcoin::Address,
        acc_addr: &monero::Address,
    ) -> Result<(), Error> {
        use config::ConfigError::Message;
        // validate arbitrating address
        match deal.arbitrating_blockchain.try_into()? {
            ArbitratingBlockchain::Bitcoin => {
                if deal.network != arb_addr.network.into() {
                    Err(Message(format!(
                        "{} address is not a {} address",
                        deal.arbitrating_blockchain, deal.network
                    )))
                } else {
                    Ok(())
                }
            }
        }?;
        // validate accordant address
        match deal.accordant_blockchain.try_into()? {
            AccordantBlockchain::Monero => {
                // Monero local address types are mainnet address types
                if deal.network != acc_addr.network.into() && deal.network != Network::Local {
                    Err(Message(format!(
                        "{} address is not a {} address",
                        deal.accordant_blockchain, deal.network
                    )))
                } else {
                    Ok(())
                }
            }
        }?;
        // all rules passed
        Ok(())
    }

    // Helper function to return default btc tradeable amounts by network
    fn btc_default_tradeable(network: Network) -> Option<TradeableAmounts<bitcoin::Amount>> {
        match network {
            Network::Mainnet => Some(Self::mainnet_btc_default_tradeable()),
            _ => None,
        }
    }

    // Helper function to return default mainnet btc tradeable amounts
    fn mainnet_btc_default_tradeable() -> TradeableAmounts<bitcoin::Amount> {
        use bitcoin::Amount;
        TradeableAmounts {
            min_amount: None,
            max_amount: Amount::from_btc(SWAP_MAINNET_BITCOIN_MAX_BTC_AMOUNT).ok(),
        }
    }

    // Helper function to return default xmr tradeable amounts by network
    fn xmr_default_tradeable(network: Network) -> Option<TradeableAmounts<monero::Amount>> {
        match network {
            Network::Mainnet => Some(Self::mainnet_xmr_default_tradeable()),
            _ => None,
        }
    }

    // Helper function to return default mainnet xmr tradeable amounts
    fn mainnet_xmr_default_tradeable() -> TradeableAmounts<monero::Amount> {
        use monero::Amount;
        TradeableAmounts {
            min_amount: Amount::from_xmr(SWAP_MAINNET_MONERO_MIN_XMR_AMOUNT).ok(),
            max_amount: Amount::from_xmr(SWAP_MAINNET_MONERO_MAX_XMR_AMOUNT).ok(),
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
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
pub struct SwapConfig {
    /// Swap parameters for the Bitcoin blockchain per network
    pub bitcoin: Networked<Option<ChainSwapConfig<ArbConfig, bitcoin::Amount>>>,
    /// Swap parameters for the Monero blockchain per network
    pub monero: Networked<Option<ChainSwapConfig<AccConfig, monero::Amount>>>,
}

/// This struct holds the complete swap config for a chain; this is just an helper
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
#[serde(bound(serialize = "T: Serialize, A: Display"))]
#[serde(bound(deserialize = "T: Deserialize<'de>, A: FromStr, A::Err: Display"))]
pub struct ChainSwapConfig<T, A>
where
    A: FromStr + Display,
    A::Err: Display,
{
    #[serde(flatten)]
    pub temporality: T,
    #[serde(flatten)]
    pub amounts: TradeableAmounts<A>,
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

#[serde_as]
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(crate = "serde_crate")]
#[serde(bound(serialize = "T: Display"))]
#[serde(bound(deserialize = "T: FromStr, T::Err: Display"))]
pub struct TradeableAmounts<T>
where
    T: FromStr + Display,
    T::Err: Display,
{
    /// If specified, the minimum acceptable amount to trade (>=)
    #[serde_as(as = "Option<DisplayFromStr>")]
    #[serde(default)]
    pub min_amount: Option<T>,
    /// If specified, the maximum acceptable amount to trade (<=)
    #[serde_as(as = "Option<DisplayFromStr>")]
    #[serde(default)]
    pub max_amount: Option<T>,
}

impl<T> TradeableAmounts<T>
where
    T: FromStr + Display,
    T::Err: Display,
{
    /// Map tradeable amounts into new type
    pub fn map<U>(self) -> TradeableAmounts<U>
    where
        T: Into<U>,
        U: FromStr + Display,
        U::Err: Display,
    {
        TradeableAmounts {
            min_amount: self.min_amount.map(|min| min.into()),
            max_amount: self.max_amount.map(|max| max.into()),
        }
    }

    /// Validate a given amount based on potential rules (min, max)
    pub fn validate_amount(&self, amount: T) -> Result<(), Error>
    where
        T: PartialOrd<T> + Copy,
    {
        use config::ConfigError::Message;
        if let Some(min) = self.min_amount {
            if amount < min {
                return Err(Message(format!("{} is smaller than {}", amount, min)))?;
            }
        }
        if let Some(max) = self.max_amount {
            if amount > max {
                return Err(Message(format!("{} is greater than {}", amount, max)))?;
            }
        }
        Ok(())
    }

    /// Helper function returning no min nor max amounts
    pub fn none() -> Self {
        Self {
            min_amount: None,
            max_amount: None,
        }
    }
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
                mainnet: Some(ChainSwapConfig {
                    temporality: ArbConfig::btc_mainnet_default(),
                    amounts: Config::mainnet_btc_default_tradeable(),
                }),
                testnet: Some(ChainSwapConfig {
                    temporality: ArbConfig::btc_testnet_default(),
                    amounts: TradeableAmounts::none(),
                }),
                local: None,
            },
            monero: Networked {
                mainnet: Some(ChainSwapConfig {
                    temporality: AccConfig::xmr_mainnet_default(),
                    amounts: Config::mainnet_xmr_default_tradeable(),
                }),
                testnet: Some(ChainSwapConfig {
                    temporality: AccConfig::xmr_testnet_default(),
                    amounts: TradeableAmounts::none(),
                }),
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
        let conf = settings.try_into::<Config>().map_err(Into::into);
        trace!("{:#?}", conf);
        conf
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
