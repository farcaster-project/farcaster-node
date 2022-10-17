use crate::walletd::runtime::{CheckpointWallet, Wallet};
use farcaster_core::blockchain::Blockchain;
use farcaster_core::swap::btcxmr::PublicOffer;
use farcaster_core::swap::SwapId;
use lmdb::{Cursor, Transaction as LMDBTransaction};
use std::convert::TryInto;
use std::path::PathBuf;
use strict_encoding::{StrictDecode, StrictEncode};

use crate::Endpoints;
use bitcoin::secp256k1::SecretKey;

use crate::bus::{
    ctl::{Checkpoint, CheckpointState, CtlMsg},
    info::{Address, InfoMsg, OfferStatusSelector},
    AddressSecretKey, BusMsg, CheckpointEntry, Failure, FailureCode, List, OfferStatus,
    OfferStatusPair, ServiceBus,
};
use crate::{CtlServer, Error, LogStyle, Service, ServiceConfig, ServiceId};
use microservices::esb::{self, Handler};

pub fn run(config: ServiceConfig, data_dir: PathBuf) -> Result<(), Error> {
    let runtime = Runtime {
        identity: ServiceId::Database,
        database: Database::new(data_dir).unwrap(),
    };

    Service::run(config, runtime, false)
}

pub struct Runtime {
    identity: ServiceId,
    database: Database,
}

impl Runtime {}

impl CtlServer for Runtime {}

impl esb::Handler<ServiceBus> for Runtime {
    type Request = BusMsg;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        self.identity.clone()
    }

    fn handle(
        &mut self,
        endpoints: &mut Endpoints,
        bus: ServiceBus,
        source: ServiceId,
        request: BusMsg,
    ) -> Result<(), Self::Error> {
        match (bus, request) {
            // Control bus for database command, only accept Ctl message
            (ServiceBus::Ctl, BusMsg::Ctl(req)) => self.handle_ctl(endpoints, source, req),
            // Info bus for client, only accept Info message
            (ServiceBus::Info, BusMsg::Info(req)) => self.handle_info(endpoints, source, req),
            // All other pairs are not supported
            (_, request) => Err(Error::NotSupported(bus, request.to_string())),
        }
    }

    fn handle_err(&mut self, _: &mut Endpoints, _: esb::Error<ServiceId>) -> Result<(), Error> {
        // We do nothing and do not propagate error; it's already being reported
        // with `error!` macro by the controller. If we propagate error here
        // this will make whole daemon panic
        Ok(())
    }
}

impl Runtime {
    fn handle_ctl(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: CtlMsg,
    ) -> Result<(), Error> {
        match request {
            CtlMsg::Hello => {
                debug!("Received Hello from {}", source);
            }

            CtlMsg::Checkpoint(Checkpoint { swap_id, state }) => {
                match state {
                    CheckpointState::CheckpointWallet(_) => {
                        debug!("setting wallet checkpoint");
                    }
                    CheckpointState::CheckpointSwapd(_) => {
                        debug!("setting swap checkpoint");
                    }
                };
                let key = CheckpointKey {
                    swap_id,
                    service_id: source,
                };
                let mut state_encoded = vec![];
                let _state_size = state.strict_encode(&mut state_encoded);
                self.database.set_checkpoint_state(&key, &state_encoded)?;
                debug!("checkpoint set");
            }

            CtlMsg::RestoreCheckpoint(CheckpointEntry { swap_id, .. }) => {
                match self.database.get_checkpoint_state(&CheckpointKey {
                    swap_id,
                    service_id: ServiceId::Wallet,
                }) {
                    Ok(raw_state) => {
                        match CheckpointState::strict_decode(std::io::Cursor::new(raw_state)) {
                            Ok(CheckpointState::CheckpointWallet(wallet)) => {
                                checkpoint_send(
                                    endpoints,
                                    swap_id,
                                    ServiceId::Database,
                                    ServiceId::Wallet,
                                    CheckpointState::CheckpointWallet(wallet),
                                )?;
                            }
                            Ok(CheckpointState::CheckpointSwapd(_)) => {
                                error!(
                                    "Decoded swapd checkpoint where walletd checkpoint was stored"
                                );
                            }
                            Err(err) => {
                                error!("Decoding the checkpoint failed: {}", err);
                            }
                        }
                    }
                    Err(err) => {
                        error!(
                            "Failed to retrieve checkpointed state for swap {}: {}",
                            swap_id, err
                        );
                    }
                }
                match self.database.get_checkpoint_state(&CheckpointKey {
                    swap_id,
                    service_id: ServiceId::Swap(swap_id),
                }) {
                    Ok(raw_state) => {
                        match CheckpointState::strict_decode(std::io::Cursor::new(raw_state)) {
                            Ok(CheckpointState::CheckpointSwapd(state)) => {
                                checkpoint_send(
                                    endpoints,
                                    swap_id,
                                    ServiceId::Database,
                                    ServiceId::Swap(swap_id),
                                    CheckpointState::CheckpointSwapd(state),
                                )?;
                            }
                            Ok(CheckpointState::CheckpointWallet(_)) => {
                                error!(
                                    "Decoded walletd checkpoint were swapd checkpoint was stored"
                                );
                            }
                            Err(err) => {
                                error!("Decoding the checkpoint failed: {}", err);
                            }
                        }
                    }
                    Err(err) => {
                        error!(
                            "Failed to retrieve checkpointed state for swap {}: {}",
                            swap_id, err
                        );
                    }
                }
            }

            CtlMsg::RemoveCheckpoint(swap_id) => {
                if let Err(err) = self.database.delete_checkpoint_state(CheckpointKey {
                    swap_id,
                    service_id: ServiceId::Wallet,
                }) {
                    debug!("{} | Did not delete checkpoint entry: {}", swap_id, err);
                }
                if let Err(err) = self.database.delete_checkpoint_state(CheckpointKey {
                    swap_id,
                    service_id: ServiceId::Swap(swap_id),
                }) {
                    debug!("{} | Did not delete checkpoint entry: {}", swap_id, err);
                }
            }

            CtlMsg::SetAddressSecretKey(AddressSecretKey::Bitcoin {
                address,
                secret_key,
            }) => {
                self.database.set_bitcoin_address(&address, &secret_key)?;
            }

            CtlMsg::SetAddressSecretKey(AddressSecretKey::Monero {
                address,
                view,
                spend,
            }) => {
                self.database
                    .set_monero_address(&address, &monero::KeyPair { view, spend })?;
            }

            CtlMsg::SetOfferStatus(OfferStatusPair { offer, status }) => {
                self.database.set_offer_status(&offer, &status)?;
            }

            _ => {
                error!("BusMsg {} is not supported by the CTL interface", request);
            }
        }

        Ok(())
    }

    fn handle_info(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: InfoMsg,
    ) -> Result<(), Error> {
        match request {
            InfoMsg::ListOffers(selector) => {
                let offer_status_pairs = self.database.get_offers(selector)?;
                self.send_client_info(
                    endpoints,
                    source,
                    InfoMsg::OfferStatusList(offer_status_pairs.into()),
                )?;
            }

            InfoMsg::RetrieveAllCheckpointInfo => {
                let pairs = self.database.get_checkpoint_key_value_pairs()?;
                let checkpointed_pub_offers: List<CheckpointEntry> = pairs
                    .iter()
                    .filter_map(|(checkpoint_key, state)| {
                        let state =
                            CheckpointState::strict_decode(std::io::Cursor::new(state)).ok()?;
                        match checkpoint_key.service_id {
                            ServiceId::Wallet => match state {
                                CheckpointState::CheckpointWallet(CheckpointWallet {
                                    wallet,
                                    ..
                                }) => match wallet {
                                    Wallet::Bob(wallet) => Some(CheckpointEntry {
                                        swap_id: checkpoint_key.swap_id,
                                        public_offer: wallet.pub_offer,
                                        trade_role: wallet.local_trade_role,
                                    }),
                                    Wallet::Alice(wallet) => Some(CheckpointEntry {
                                        swap_id: checkpoint_key.swap_id,
                                        public_offer: wallet.pub_offer,
                                        trade_role: wallet.local_trade_role,
                                    }),
                                },
                                s => {
                                    error!(
                                        "Checkpoint {} not supported for service {}",
                                        s,
                                        ServiceId::Wallet
                                    );
                                    None
                                }
                            },
                            _ => None,
                        }
                    })
                    .collect();
                endpoints.send_to(
                    ServiceBus::Info,
                    self.identity(),
                    source,
                    BusMsg::Info(InfoMsg::CheckpointList(checkpointed_pub_offers)),
                )?;
            }

            InfoMsg::GetCheckpointEntry(swap_id) => {
                let entry = match self.database.get_checkpoint_state(&CheckpointKey {
                    swap_id,
                    service_id: ServiceId::Wallet,
                }) {
                    Ok(raw_state) => {
                        match CheckpointState::strict_decode(std::io::Cursor::new(raw_state)) {
                            Ok(CheckpointState::CheckpointWallet(CheckpointWallet {
                                wallet,
                                ..
                            })) => match wallet {
                                Wallet::Bob(wallet) => Some(CheckpointEntry {
                                    swap_id,
                                    public_offer: wallet.pub_offer,
                                    trade_role: wallet.local_trade_role,
                                }),
                                Wallet::Alice(wallet) => Some(CheckpointEntry {
                                    swap_id,
                                    public_offer: wallet.pub_offer,
                                    trade_role: wallet.local_trade_role,
                                }),
                            },
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some(entry) = entry {
                    endpoints.send_to(
                        ServiceBus::Info,
                        self.identity(),
                        source,
                        BusMsg::Info(InfoMsg::CheckpointEntry(entry)),
                    )?;
                } else {
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        source,
                        BusMsg::Ctl(CtlMsg::Failure(Failure {
                            code: FailureCode::Unknown,
                            info: format!("Could not retrieve checkpoint entry for {}", swap_id),
                        })),
                    )?;
                }
            }

            InfoMsg::GetAddressSecretKey(Address::Monero(address)) => {
                match self.database.get_monero_address_secret_key(&address) {
                    Err(_) => endpoints.send_to(
                        ServiceBus::Ctl,
                        ServiceId::Database,
                        source,
                        BusMsg::Ctl(CtlMsg::Failure(Failure {
                            code: FailureCode::Unknown,
                            info: format!("Could not retrieve secret key for address {}", address),
                        })),
                    )?,
                    Ok(secret_key_pair) => {
                        endpoints.send_to(
                            ServiceBus::Info,
                            ServiceId::Database,
                            source,
                            BusMsg::Info(InfoMsg::AddressSecretKey(AddressSecretKey::Monero {
                                address,
                                view: secret_key_pair.view.as_bytes().try_into().unwrap(),
                                spend: secret_key_pair.spend.as_bytes().try_into().unwrap(),
                            })),
                        )?;
                    }
                }
            }

            InfoMsg::GetAddressSecretKey(Address::Bitcoin(address)) => {
                match self.database.get_bitcoin_address_secret_key(&address) {
                    Err(_) => endpoints.send_to(
                        ServiceBus::Ctl,
                        ServiceId::Database,
                        source,
                        BusMsg::Ctl(CtlMsg::Failure(Failure {
                            code: FailureCode::Unknown,
                            info: format!("Could not retrieve secret key for address {}", address),
                        })),
                    )?,
                    Ok(secret_key) => {
                        endpoints.send_to(
                            ServiceBus::Info,
                            ServiceId::Database,
                            source,
                            BusMsg::Info(InfoMsg::AddressSecretKey(AddressSecretKey::Bitcoin {
                                address,
                                secret_key,
                            })),
                        )?;
                    }
                }
            }

            InfoMsg::GetAddresses(Blockchain::Bitcoin) => {
                let addresses = self.database.get_all_bitcoin_addresses()?;
                endpoints.send_to(
                    ServiceBus::Info,
                    ServiceId::Database,
                    source,
                    BusMsg::Info(InfoMsg::BitcoinAddressList(addresses.into())),
                )?;
            }

            InfoMsg::GetAddresses(Blockchain::Monero) => {
                let addresses = self.database.get_all_monero_addresses()?;
                endpoints.send_to(
                    ServiceBus::Info,
                    ServiceId::Database,
                    source,
                    BusMsg::Info(InfoMsg::MoneroAddressList(addresses.into())),
                )?;
            }

            req => {
                warn!("Ignoring request: {}", req.err());
            }
        }

        Ok(())
    }
}

pub fn checkpoint_send(
    endpoints: &mut Endpoints,
    swap_id: SwapId,
    source: ServiceId,
    destination: ServiceId,
    state: CheckpointState,
) -> Result<(), Error> {
    endpoints.send_to(
        ServiceBus::Ctl,
        source,
        destination,
        BusMsg::Ctl(CtlMsg::Checkpoint(Checkpoint { swap_id, state })),
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
struct CheckpointKey {
    swap_id: SwapId,
    service_id: ServiceId,
}

impl From<CheckpointKey> for Vec<u8> {
    fn from(checkpoint_key: CheckpointKey) -> Self {
        Into::<[u8; 32]>::into(checkpoint_key.swap_id)
            .iter()
            .cloned()
            .chain(
                Into::<Vec<u8>>::into(checkpoint_key.service_id)
                    .iter()
                    .cloned(),
            )
            .collect()
    }
}

impl From<Vec<u8>> for CheckpointKey {
    fn from(raw_key: Vec<u8>) -> Self {
        CheckpointKey {
            swap_id: SwapId(raw_key[0..32].try_into().unwrap()),
            service_id: raw_key[32..].to_vec().into(),
        }
    }
}

struct Database(lmdb::Environment);

const LMDB_CHECKPOINTS: &str = "checkpoints";
const LMDB_BITCOIN_ADDRESSES: &str = "bitcoin_addresses";
const LMDB_MONERO_ADDRESSES: &str = "monero_addresses";
const LMDB_OFFER_HISTORY: &str = "offer_history";

impl Database {
    fn new(path: PathBuf) -> Result<Database, lmdb::Error> {
        let env = lmdb::Environment::new()
            .set_map_size(10485760 * 1024 * 64)
            .set_max_dbs(4)
            .open(&path)?;
        env.create_db(Some(LMDB_CHECKPOINTS), lmdb::DatabaseFlags::empty())?;
        env.create_db(Some(LMDB_BITCOIN_ADDRESSES), lmdb::DatabaseFlags::empty())?;
        env.create_db(Some(LMDB_OFFER_HISTORY), lmdb::DatabaseFlags::empty())?;
        env.create_db(Some(LMDB_MONERO_ADDRESSES), lmdb::DatabaseFlags::empty())?;
        Ok(Database(env))
    }

    fn set_offer_status(
        &mut self,
        offer: &PublicOffer,
        status: &OfferStatus,
    ) -> Result<(), lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_OFFER_HISTORY))?;
        let mut tx = self.0.begin_rw_txn()?;
        let mut key = vec![];
        let _key_size = offer.strict_encode(&mut key);
        if tx.get(db, &key).is_ok() {
            tx.del(db, &key.clone(), None)?;
        }
        let mut val = vec![];
        let _key_size = status.strict_encode(&mut val);
        tx.put(db, &key, &val, lmdb::WriteFlags::empty())?;
        tx.commit()?;
        Ok(())
    }

    fn get_offers(
        &mut self,
        selector: OfferStatusSelector,
    ) -> Result<Vec<OfferStatusPair>, lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_OFFER_HISTORY))?;
        let tx = self.0.begin_ro_txn()?;
        let mut cursor = tx.open_ro_cursor(db)?;
        let res = cursor
            .iter()
            .filter_map(|(key, val)| {
                let status = OfferStatus::strict_decode(std::io::Cursor::new(val.to_vec())).ok()?;
                let filtered_status = match status {
                    OfferStatus::Open if selector == OfferStatusSelector::Open => Some(status),
                    OfferStatus::InProgress if selector == OfferStatusSelector::InProgress => {
                        Some(status)
                    }
                    OfferStatus::Ended(_) if selector == OfferStatusSelector::Ended => Some(status),
                    _ if selector == OfferStatusSelector::All => Some(status),
                    _ => None,
                }?;
                let offer = PublicOffer::strict_decode(std::io::Cursor::new(key.to_vec())).ok()?;
                Some(OfferStatusPair {
                    offer,
                    status: filtered_status,
                })
            })
            .collect();
        Ok(res)
    }

    fn set_bitcoin_address(
        &mut self,
        address: &bitcoin::Address,
        secret_key: &SecretKey,
    ) -> Result<(), lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_BITCOIN_ADDRESSES))?;
        let mut tx = self.0.begin_rw_txn()?;
        let mut key = vec![];
        let _key_size = address.strict_encode(&mut key);
        if tx.get(db, &key).is_err() {
            tx.put(
                db,
                &key,
                &secret_key.secret_bytes(),
                lmdb::WriteFlags::empty(),
            )?;
        } else {
            warn!(
                "address {} was already persisted with its secret key",
                address
            );
        }
        tx.commit()?;
        Ok(())
    }

    fn get_bitcoin_address_secret_key(
        &mut self,
        address: &bitcoin::Address,
    ) -> Result<SecretKey, lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_BITCOIN_ADDRESSES))?;
        let tx = self.0.begin_ro_txn()?;
        let mut key = vec![];
        let _key_size = address.strict_encode(&mut key);
        let val = SecretKey::from_slice(tx.get(db, &key)?)
            .expect("we only insert private keys, so retrieving one should not fail");
        tx.abort();
        Ok(val)
    }

    fn get_all_bitcoin_addresses(&mut self) -> Result<Vec<bitcoin::Address>, lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_BITCOIN_ADDRESSES))?;
        let tx = self.0.begin_ro_txn()?;
        let mut cursor = tx.open_ro_cursor(db)?;
        let res = cursor
            .iter()
            .filter_map(|(key, _)| {
                bitcoin::Address::strict_decode(std::io::Cursor::new(key.to_vec())).ok()
            })
            .collect();
        drop(cursor);
        tx.abort();
        Ok(res)
    }

    fn set_monero_address(
        &mut self,
        address: &monero::Address,
        secret_keys: &monero::KeyPair,
    ) -> Result<(), lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_BITCOIN_ADDRESSES))?;
        let mut tx = self.0.begin_rw_txn()?;
        let key = address.as_bytes();
        let mut val = secret_keys.spend.as_bytes().to_vec();
        val.append(&mut secret_keys.view.as_bytes().to_vec());
        if tx.get(db, &key).is_err() {
            tx.put(db, &key, &val, lmdb::WriteFlags::empty())?;
        } else {
            warn!(
                "address {} was already persisted with its secret key",
                address
            );
        }
        tx.commit()?;
        Ok(())
    }

    fn get_monero_address_secret_key(
        &mut self,
        address: &monero::Address,
    ) -> Result<monero::KeyPair, lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_BITCOIN_ADDRESSES))?;
        let tx = self.0.begin_ro_txn()?;
        let key = address.as_bytes();
        let val: [u8; 64] = tx
            .get(db, &key)?
            .try_into()
            .expect("every monero address should have a keypair");
        tx.abort();
        Ok(monero::KeyPair {
            spend: val[0..32].try_into().expect("unable to decode spend key"),
            view: val[32..64].try_into().expect("unable to decode view key"),
        })
    }

    fn get_all_monero_addresses(&mut self) -> Result<Vec<String>, lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_BITCOIN_ADDRESSES))?;
        let tx = self.0.begin_ro_txn()?;
        let mut cursor = tx.open_ro_cursor(db)?;
        let res = cursor
            .iter()
            .filter_map(|(key, _)| monero::Address::from_bytes(key).ok())
            .map(|addr| addr.to_string())
            .collect();
        drop(cursor);
        tx.abort();
        Ok(res)
    }

    fn set_checkpoint_state(&mut self, key: &CheckpointKey, val: &[u8]) -> Result<(), lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_CHECKPOINTS))?;
        let mut tx = self.0.begin_rw_txn()?;
        if tx.get(db, &Vec::from(key.clone())).is_ok() {
            tx.del(db, &Vec::from(key.clone()), None)?;
        }
        tx.put(db, &Vec::from(key.clone()), &val, lmdb::WriteFlags::empty())?;
        tx.commit()?;
        Ok(())
    }

    fn get_checkpoint_state(&mut self, key: &CheckpointKey) -> Result<Vec<u8>, lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_CHECKPOINTS))?;
        let tx = self.0.begin_ro_txn()?;
        let val: Vec<u8> = tx.get(db, &Vec::from(key.clone()))?.into();
        tx.abort();
        Ok(val)
    }

    fn delete_checkpoint_state(&mut self, key: CheckpointKey) -> Result<(), lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_CHECKPOINTS))?;
        let mut tx = self.0.begin_rw_txn()?;
        tx.del(db, &Vec::from(key), None)?;
        tx.commit()?;
        Ok(())
    }

    fn get_checkpoint_key_value_pairs(
        &mut self,
    ) -> Result<Vec<(CheckpointKey, Vec<u8>)>, lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_CHECKPOINTS))?;
        let tx = self.0.begin_ro_txn()?;
        let mut cursor = tx.open_ro_cursor(db)?;
        let res = cursor
            .iter()
            .map(|(key, value)| (CheckpointKey::from(key.to_vec()), value.to_vec()))
            .collect();
        drop(cursor);
        tx.abort();
        Ok(res)
    }
}

#[test]
fn test_lmdb_state() {
    use crate::bus::Outcome;
    use std::str::FromStr;

    let val1 = vec![0, 1];
    let val2 = vec![2, 3, 4, 5];
    let key1 = CheckpointKey {
        swap_id: SwapId::random(),
        service_id: ServiceId::Swap(SwapId::random()),
    };
    let key2 = CheckpointKey {
        swap_id: SwapId::random(),
        service_id: ServiceId::Database,
    };
    let path = std::env::current_dir().unwrap();
    let mut database = Database::new(path.to_path_buf()).unwrap();
    database.set_checkpoint_state(&key1, &val1).unwrap();
    let res = database.get_checkpoint_state(&key1).unwrap();
    assert_eq!(val1, res);
    database.set_checkpoint_state(&key1, &val2).unwrap();
    let res = database.get_checkpoint_state(&key1).unwrap();
    assert_eq!(val2, res);
    database.set_checkpoint_state(&key2, &val2).unwrap();
    let res = database.get_checkpoint_state(&key2).unwrap();
    assert_eq!(val2, res);
    database.delete_checkpoint_state(key2.clone()).unwrap();
    let res = database.get_checkpoint_state(&key2);
    assert!(res.is_err());
    database.get_checkpoint_key_value_pairs().unwrap();

    let sk = SecretKey::new(&mut bitcoin::secp256k1::rand::thread_rng());
    let private_key =
        bitcoin::PrivateKey::from_slice(&sk.secret_bytes(), bitcoin::Network::Testnet).unwrap();
    let pk = bitcoin::PublicKey::from_private_key(bitcoin::secp256k1::SECP256K1, &private_key);
    let addr = bitcoin::Address::p2wpkh(&pk, bitcoin::Network::Testnet).unwrap();
    database.set_bitcoin_address(&addr, &sk).unwrap();
    let val_retrieved = database.get_bitcoin_address_secret_key(&addr).unwrap();
    assert_eq!(sk, val_retrieved);
    let addrs = database.get_all_bitcoin_addresses().unwrap();
    assert!(addrs.contains(&addr));

    let key_pair = monero::KeyPair {
        spend: monero::PrivateKey::from_str(
            "77916d0cd56ed1920aef6ca56d8a41bac915b68e4c46a589e0956e27a7b77404",
        )
        .unwrap(),
        view: monero::PrivateKey::from_str(
            "8163466f1883598e6dd14027b8da727057165da91485834314f5500a65846f09",
        )
        .unwrap(),
    };
    let addr = monero::Address::from_keypair(monero::Network::Stagenet, &key_pair);
    database.set_monero_address(&addr, &key_pair).unwrap();
    let val_retrieved = database.get_monero_address_secret_key(&addr).unwrap();
    assert_eq!(key_pair, val_retrieved);
    let addrs = database.get_all_monero_addresses().unwrap();
    assert!(addrs.contains(&addr.to_string()));

    let offer_1 = PublicOffer::from_str("Offer:Cke4ftrP5A7MgLMaQZLZUMTC6TfkqUKBu1LQM2fvVdFMNR4gmBqNCsR11111uMFuZTAsNgpdK8DiK11111TB9zym113GTvtvqfD1111114A4TTF4h53Tv4MR6eS9sdDxV5JCH9xZcKejCqKShnphqndeeD11111111111111111111111111111111111111111AfZ113XRBtrLeA3t").unwrap();
    let offer_2 = PublicOffer::from_str("Offer:Cke4ftrP5A7Km9Kmc2UDBePio1p7wM56P1LQM2fvVdFMNR4gmBqNCsR11111uMFuZTAsNgpdK8DiK11111TB9zym113GTvtvqfD1111114A4TTF4h53Tv4MR6eS9sdDxV5JCH9xZcKejCqKShnphqndeeD11111111111111111111111111111111111111111AfZ113XRBuLWyw3M").unwrap();

    database
        .set_offer_status(&offer_1, &OfferStatus::Open)
        .unwrap();
    let offers_retrieved = database.get_offers(OfferStatusSelector::All).unwrap();
    assert_eq!(offer_1, offers_retrieved[0].offer);

    let offers_retrieved = database.get_offers(OfferStatusSelector::Open).unwrap();
    assert_eq!(offer_1, offers_retrieved[0].offer);

    database
        .set_offer_status(&offer_1, &OfferStatus::InProgress)
        .unwrap();
    let offers_retrieved = database
        .get_offers(OfferStatusSelector::InProgress)
        .unwrap();
    assert_eq!(offer_1, offers_retrieved[0].offer);

    database
        .set_offer_status(&offer_1, &OfferStatus::Ended(Outcome::Buy))
        .unwrap();
    let offers_retrieved = database.get_offers(OfferStatusSelector::Ended).unwrap();
    assert_eq!(offer_1, offers_retrieved[0].offer);

    database
        .set_offer_status(&offer_2, &OfferStatus::Open)
        .unwrap();
    let offers_retrieved = database.get_offers(OfferStatusSelector::All).unwrap();
    let status_1 = OfferStatusPair {
        offer: offer_1,
        status: OfferStatus::Ended(Outcome::Buy),
    };
    let status_2 = OfferStatusPair {
        offer: offer_2,
        status: OfferStatus::Open,
    };
    assert!(offers_retrieved.len() == 2);
    assert!(offers_retrieved.contains(&status_1));
    assert!(offers_retrieved.contains(&status_2));
}
