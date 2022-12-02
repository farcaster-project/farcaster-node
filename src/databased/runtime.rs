use crate::bus::{BitcoinSecretKeyInfo, MoneroSecretKeyInfo, Outcome};
use farcaster_core::blockchain::Blockchain;
use farcaster_core::swap::btcxmr::PublicOffer;
use farcaster_core::swap::SwapId;
use lmdb::{Cursor, Transaction as LMDBTransaction};
use std::convert::TryInto;
use std::path::PathBuf;
use strict_encoding::{StrictDecode, StrictEncode};

use crate::bus::{
    ctl::{Checkpoint, CheckpointState, CtlMsg},
    info::{Address, InfoMsg, OfferStatusSelector},
    AddressSecretKey, BusMsg, CheckpointEntry, Failure, FailureCode, List, OfferStatus,
    OfferStatusPair, ServiceBus,
};
use crate::Endpoints;
use crate::{CtlServer, Error, LogStyle, Service, ServiceConfig, ServiceId};
use microservices::esb::{self};

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
                match &state {
                    CheckpointState::CheckpointWallet(_) => {
                        debug!("setting wallet checkpoint");
                    }
                    CheckpointState::CheckpointSwapd(swap_checkpoint) => {
                        let info = CheckpointEntry {
                            swap_id,
                            public_offer: swap_checkpoint.public_offer.clone(),
                            trade_role: swap_checkpoint.local_trade_role.clone(),
                            expected_counterparty_node_id: swap_checkpoint
                                .connected_counterparty_node_id
                                .clone(),
                        };
                        debug!("setting checkpoint info entry");
                        let mut info_encoded = vec![];
                        info.strict_encode(&mut info_encoded)?;
                        self.database.set_checkpoint_info(&swap_id, &info_encoded)?;

                        debug!("setting swap checkpoint");
                    }
                };
                let key = CheckpointKey {
                    swap_id,
                    service_id: source,
                };
                let mut state_encoded = vec![];
                state.strict_encode(&mut state_encoded)?;
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
                    debug!(
                        "{} | Did not delete checkpoint wallet entry: {}",
                        swap_id, err
                    );
                }
                if let Err(err) = self.database.delete_checkpoint_state(CheckpointKey {
                    swap_id,
                    service_id: ServiceId::Swap(swap_id),
                }) {
                    debug!(
                        "{} | Did not delete checkpoint swap entry: {}",
                        swap_id, err
                    );
                }
                if let Err(err) = self.database.delete_checkpoint_info(swap_id) {
                    debug!("{} | Did not delete checkpoint info: {}", swap_id, err);
                }
            }

            CtlMsg::SetAddressSecretKey(AddressSecretKey::Bitcoin {
                address,
                secret_key_info,
            }) => {
                self.database
                    .set_bitcoin_address(&address, &secret_key_info)?;
            }

            CtlMsg::SetAddressSecretKey(AddressSecretKey::Monero {
                address,
                secret_key_info,
            }) => {
                self.database
                    .set_monero_address(&address, &secret_key_info)?;
            }

            CtlMsg::SetOfferStatus(OfferStatusPair { offer, status }) => {
                self.database.set_offer_status(&offer, &status)?;
            }

            CtlMsg::CleanDanglingOffers => {
                let checkpointed_pub_offers: Vec<PublicOffer> = self
                    .database
                    .get_all_checkpoint_info()?
                    .iter()
                    .filter_map(|(_, info)| {
                        CheckpointEntry::strict_decode(std::io::Cursor::new(info)).ok()
                    })
                    .map(|c| c.public_offer)
                    .collect();
                self.database
                    .get_offers(OfferStatusSelector::InProgress)?
                    .drain(..)
                    .filter_map(|o| {
                        if !checkpointed_pub_offers.contains(&o.offer) {
                            Some(o.offer)
                        } else {
                            None
                        }
                    })
                    .map(|offer| {
                        self.database
                            .set_offer_status(&offer, &OfferStatus::Ended(Outcome::FailureAbort))
                    })
                    .collect::<Result<_, _>>()?;
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
                let pairs = self.database.get_all_checkpoint_info()?;
                let checkpointed_pub_offers: List<CheckpointEntry> = pairs
                    .iter()
                    .filter_map(|(_, info)| {
                        CheckpointEntry::strict_decode(std::io::Cursor::new(info)).ok()
                    })
                    .collect();
                self.send_client_info(
                    endpoints,
                    source,
                    InfoMsg::CheckpointList(checkpointed_pub_offers),
                )?;
            }

            InfoMsg::GetCheckpointEntry(swap_id) => {
                let entry = match self.database.get_checkpoint_info(&swap_id) {
                    Ok(raw_state) => {
                        match CheckpointEntry::strict_decode(std::io::Cursor::new(raw_state)) {
                            Ok(info) => Some(info),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some(entry) = entry {
                    self.send_client_info(endpoints, source, InfoMsg::CheckpointEntry(entry))?;
                } else {
                    self.send_client_ctl(
                        endpoints,
                        source,
                        CtlMsg::Failure(Failure {
                            code: FailureCode::Unknown,
                            info: format!("Could not retrieve checkpoint entry for {}", swap_id),
                        }),
                    )?;
                }
            }

            InfoMsg::GetAddressSecretKey(Address::Monero(address)) => {
                match self.database.get_monero_address_secret_key(&address) {
                    Err(_) => {
                        self.send_client_ctl(
                            endpoints,
                            source,
                            CtlMsg::Failure(Failure {
                                code: FailureCode::Unknown,
                                info: format!(
                                    "Could not retrieve secret key for address {}",
                                    address
                                ),
                            }),
                        )?;
                    }
                    Ok(secret_key_info) => {
                        self.send_client_info(
                            endpoints,
                            source,
                            InfoMsg::AddressSecretKey(AddressSecretKey::Monero {
                                address,
                                secret_key_info,
                            }),
                        )?;
                    }
                }
            }

            InfoMsg::GetAddressSecretKey(Address::Bitcoin(address)) => {
                match self.database.get_bitcoin_address_secret_key(&address) {
                    Err(_) => self.send_client_ctl(
                        endpoints,
                        source,
                        CtlMsg::Failure(Failure {
                            code: FailureCode::Unknown,
                            info: format!("Could not retrieve secret key for address {}", address),
                        }),
                    )?,
                    Ok(secret_key_info) => {
                        self.send_client_info(
                            endpoints,
                            source,
                            InfoMsg::AddressSecretKey(AddressSecretKey::Bitcoin {
                                address,
                                secret_key_info,
                            }),
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
const LMDB_CHECKPOINT_INFOS: &str = "checkpoint_infos";
const LMDB_BITCOIN_ADDRESSES: &str = "bitcoin_addresses";
const LMDB_MONERO_ADDRESSES: &str = "monero_addresses";
const LMDB_OFFER_HISTORY: &str = "offer_history";

impl Database {
    fn new(path: PathBuf) -> Result<Database, lmdb::Error> {
        let env = lmdb::Environment::new()
            .set_map_size(10485760 * 1024 * 64)
            .set_max_dbs(10)
            .open(&path)?;
        env.create_db(Some(LMDB_CHECKPOINTS), lmdb::DatabaseFlags::empty())?;
        env.create_db(Some(LMDB_CHECKPOINT_INFOS), lmdb::DatabaseFlags::empty())?;
        env.create_db(Some(LMDB_BITCOIN_ADDRESSES), lmdb::DatabaseFlags::empty())?;
        env.create_db(Some(LMDB_OFFER_HISTORY), lmdb::DatabaseFlags::empty())?;
        env.create_db(Some(LMDB_MONERO_ADDRESSES), lmdb::DatabaseFlags::empty())?;
        Ok(Database(env))
    }

    fn set_offer_status(&mut self, offer: &PublicOffer, status: &OfferStatus) -> Result<(), Error> {
        let db = self.0.open_db(Some(LMDB_OFFER_HISTORY))?;
        let mut tx = self.0.begin_rw_txn()?;
        let mut key = vec![];
        offer.strict_encode(&mut key)?;
        if tx.get(db, &key).is_ok() {
            tx.del(db, &key, None)?;
        }
        let mut val = vec![];
        status.strict_encode(&mut val)?;
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
        secret_key_info: &BitcoinSecretKeyInfo,
    ) -> Result<(), Error> {
        let db = self.0.open_db(Some(LMDB_BITCOIN_ADDRESSES))?;
        let mut tx = self.0.begin_rw_txn()?;
        let mut key = vec![];
        address.strict_encode(&mut key)?;
        let mut val = vec![];
        secret_key_info.strict_encode(&mut val)?;
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

    fn get_bitcoin_address_secret_key(
        &mut self,
        address: &bitcoin::Address,
    ) -> Result<BitcoinSecretKeyInfo, Error> {
        let db = self.0.open_db(Some(LMDB_BITCOIN_ADDRESSES))?;
        let tx = self.0.begin_ro_txn()?;
        let mut key = vec![];
        address.strict_encode(&mut key)?;
        let val = BitcoinSecretKeyInfo::strict_decode(tx.get(db, &key)?)?;
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
        secret_key_info: &MoneroSecretKeyInfo,
    ) -> Result<(), Error> {
        let db = self.0.open_db(Some(LMDB_BITCOIN_ADDRESSES))?;
        let mut tx = self.0.begin_rw_txn()?;
        let key = address.as_bytes();
        let mut val = vec![];
        secret_key_info.strict_encode(&mut val)?;
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
    ) -> Result<MoneroSecretKeyInfo, Error> {
        let db = self.0.open_db(Some(LMDB_BITCOIN_ADDRESSES))?;
        let tx = self.0.begin_ro_txn()?;
        let key = address.as_bytes();
        let val = MoneroSecretKeyInfo::strict_decode(tx.get(db, &key)?)?;
        tx.abort();
        Ok(val)
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

    fn set_checkpoint_info(&mut self, key: &SwapId, val: &[u8]) -> Result<(), lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_CHECKPOINT_INFOS))?;
        let mut tx = self.0.begin_rw_txn()?;
        if tx.get(db, &key.as_bytes()).is_ok() {
            tx.del(db, &key.as_bytes(), None)?;
        }
        tx.put(db, &key.as_bytes(), &val, lmdb::WriteFlags::empty())?;
        tx.commit()?;
        Ok(())
    }

    fn get_checkpoint_info(&mut self, key: &SwapId) -> Result<Vec<u8>, lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_CHECKPOINT_INFOS))?;
        let tx = self.0.begin_ro_txn()?;
        let val: Vec<u8> = tx.get(db, &key.as_bytes())?.into();
        tx.abort();
        Ok(val)
    }

    fn get_all_checkpoint_info(&mut self) -> Result<Vec<(SwapId, Vec<u8>)>, lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_CHECKPOINT_INFOS))?;
        let tx = self.0.begin_ro_txn()?;
        let mut cursor = tx.open_ro_cursor(db)?;
        let res = cursor
            .iter()
            .map(|(key, value)| (SwapId::from_slice(key), value.to_vec()))
            .collect();
        drop(cursor);
        tx.abort();
        Ok(res)
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

    fn delete_checkpoint_info(&mut self, key: SwapId) -> Result<(), lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_CHECKPOINT_INFOS))?;
        let mut tx = self.0.begin_rw_txn()?;
        tx.del(db, &key.as_bytes(), None)?;
        tx.commit()?;
        Ok(())
    }
}

#[test]
fn test_lmdb_state() {
    use crate::bus::Outcome;
    use bitcoin::secp256k1::SecretKey;
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

    let key_info = SwapId::random();
    let val_info = vec![0, 1, 2];
    database.set_checkpoint_info(&key_info, &val_info).unwrap();
    let res = database.get_checkpoint_info(&key_info).unwrap();
    assert_eq!(val_info, res);
    database.delete_checkpoint_info(key_info).unwrap();

    let sk = SecretKey::new(&mut bitcoin::secp256k1::rand::thread_rng());
    let private_key =
        bitcoin::PrivateKey::from_slice(&sk.secret_bytes(), bitcoin::Network::Testnet).unwrap();
    let pk = bitcoin::PublicKey::from_private_key(bitcoin::secp256k1::SECP256K1, &private_key);
    let addr = bitcoin::Address::p2wpkh(&pk, bitcoin::Network::Testnet).unwrap();
    let addr_info = BitcoinSecretKeyInfo {
        swap_id: None,
        secret_key: sk,
    };
    database.set_bitcoin_address(&addr, &addr_info).unwrap();
    let val_retrieved = database.get_bitcoin_address_secret_key(&addr).unwrap();
    assert_eq!(addr_info, val_retrieved);
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
    let addr_info = MoneroSecretKeyInfo {
        swap_id: None,
        creation_height: None,
        view: key_pair.view,
        spend: key_pair.spend,
    };

    let addr = monero::Address::from_keypair(monero::Network::Stagenet, &key_pair);
    database.set_monero_address(&addr, &addr_info).unwrap();
    let val_retrieved = database.get_monero_address_secret_key(&addr).unwrap();
    assert_eq!(addr_info, val_retrieved);
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
        .set_offer_status(&offer_1, &OfferStatus::Ended(Outcome::SuccessSwap))
        .unwrap();
    let offers_retrieved = database.get_offers(OfferStatusSelector::Ended).unwrap();
    assert_eq!(offer_1, offers_retrieved[0].offer);

    database
        .set_offer_status(&offer_2, &OfferStatus::Open)
        .unwrap();
    let offers_retrieved = database.get_offers(OfferStatusSelector::All).unwrap();
    let status_1 = OfferStatusPair {
        offer: offer_1,
        status: OfferStatus::Ended(Outcome::SuccessSwap),
    };
    let status_2 = OfferStatusPair {
        offer: offer_2,
        status: OfferStatus::Open,
    };
    assert!(offers_retrieved.len() == 2);
    assert!(offers_retrieved.contains(&status_1));
    assert!(offers_retrieved.contains(&status_2));
}
