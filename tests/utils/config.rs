//! Farcaster configuration management module for functional tests.
//!
//! Holds the `TestConfig` structure used to setup all needed infrastructure to run functional
//! tests. It allows to parse the configuration from a yaml file and get the correct configuration
//! based on the context: ci or (docker-)compose.

use bitcoincore_rpc::Auth;
use serde::{Deserialize, Serialize};
use serde_crate as serde;
use std::env;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(crate = "serde_crate")]
struct FullConfig {
    ci: TestConfig,
    compose: TestConfig,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(crate = "serde_crate")]
pub struct TestConfig {
    pub bitcoin: BitcoinConfig,
    pub electrs: NodeConfig,
    pub monero: MoneroConfig,
}

impl TestConfig {
    pub fn parse() -> Self {
        let s = fs::read_to_string("./tests/cfg/config.yml")
            .expect("Invalid configuration path provided!");
        let conf: FullConfig =
            serde_yaml::from_str(&s).expect("Invalid configuration format used!");

        let ctx = env::var("CI").unwrap_or_else(|_| "false".into());
        if ctx == "true" {
            info!("configuration used(CI): {:#?}", conf.ci);
            conf.ci
        } else {
            info!("configuration used(COMPOSE): {:#?}", conf.compose);
            conf.compose
        }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(crate = "serde_crate")]
pub struct NodeConfig {
    pub host: String,
    pub port: u16,
    pub protocol: Option<String>,
}

impl fmt::Display for NodeConfig {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let Some(proto) = &self.protocol {
            write!(f, "{}://", proto)?;
        }
        write!(f, "{}:{}", self.host, self.port)
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(crate = "serde_crate")]
pub struct BitcoinConfig {
    pub daemon: NodeConfig,
    pub auth: BitcoinAuthConfig,
}

impl BitcoinConfig {
    /// Returns an `Auth` based on values provided in the configuration file. If all values are
    /// provided the Cookie method is prefered.
    pub fn get_auth(&self) -> Auth {
        self.auth.get_auth()
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(crate = "serde_crate")]
pub struct BitcoinAuthConfig {
    pub cookie: Option<String>,
    pub user: Option<String>,
    pub pass: Option<String>,
}

impl BitcoinAuthConfig {
    /// Returns an `Auth` based on values provided in the configuration file. If all values are
    /// provided the Cookie method is prefered.
    pub fn get_auth(&self) -> Auth {
        if let Some(cookie) = &self.cookie {
            let path = PathBuf::from_str(cookie).expect("Invalid path given!");
            return Auth::CookieFile(path);
        } else if let Some(user) = &self.user {
            if let Some(pass) = &self.pass {
                return Auth::UserPass(user.to_string(), pass.to_string());
            }
        }
        panic!("No authentification method provided!");
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(crate = "serde_crate")]
pub struct MoneroConfig {
    pub daemon: NodeConfig,
    pub wallets: Vec<NodeConfig>,
    pub lws: NodeConfig,
}

impl MoneroConfig {
    /// Utility function to directly retrieve a type of wallet, panic if the wallet is not found in
    /// the configuration.
    ///
    /// ## SAFETY
    /// This function is intended to be used in a tests context; if the node config is not found in
    /// the list the function will panic, failing the test.
    pub fn get_wallet(&self, idx: WalletIndex) -> &NodeConfig {
        self.wallets
            .get(usize::from(idx))
            .expect("The wallet requested does not exists in the config file!")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletIndex {
    Primary,
    Secondary,
    Tertiary,
}

impl From<WalletIndex> for usize {
    fn from(idx: WalletIndex) -> usize {
        match idx {
            WalletIndex::Primary => 0,
            WalletIndex::Secondary => 1,
            WalletIndex::Tertiary => 2,
        }
    }
}
