use crate::databased::runtime::request::OfferStatus;
use crate::databased::runtime::request::OfferStatusPair;
use crate::databased::runtime::request::OfferStatusSelector;
use crate::farcaster_core::consensus::Encodable;
use crate::walletd::runtime::{CheckpointWallet, Wallet};
use farcaster_core::negotiation::PublicOffer;
use farcaster_core::swap::btcxmr::BtcXmr;
use farcaster_core::swap::SwapId;
use lmdb::{Cursor, Transaction as LMDBTransaction};
use microservices::rpc::Failure;
use std::path::PathBuf;
use std::{
    any::Any,
    collections::{HashMap, HashSet},
    convert::{TryFrom, TryInto},
    io::{self, Write},
    ptr::swap_nonoverlapping,
    str::FromStr,
};
use strict_encoding::StrictDecode;
use strict_encoding::StrictEncode;

use crate::swapd::get_swap_id;
use crate::Endpoints;
use crate::LogStyle;
use bitcoin::hashes::{ripemd160, Hash};
use bitcoin::secp256k1::SecretKey;

use crate::{
    rpc::{
        request::{
            self, BitcoinAddress, Checkpoint, CheckpointChunk, CheckpointEntry,
            CheckpointMultipartChunk, CheckpointState, Commit, Keys, LaunchSwap, List,
            MoneroAddress, Msg, NodeId, Params, Reveal, Token, Tx,
        },
        Request, ServiceBus,
    },
    syncerd::SweepXmrAddress,
};
use crate::{CtlServer, Error, Service, ServiceConfig, ServiceId};
use colored::Colorize;
use internet2::TypedEnum;
use microservices::esb::{self, Handler};

pub fn run(config: ServiceConfig, data_dir: PathBuf) -> Result<(), Error> {
    let runtime = Runtime {
        identity: ServiceId::Database,
        database: Database::new(data_dir).unwrap(),
        pending_checkpoint_chunks: map![],
    };

    Service::run(config, runtime, false)
}

pub struct Runtime {
    identity: ServiceId,
    database: Database,
    pending_checkpoint_chunks: HashMap<[u8; 20], HashSet<CheckpointChunk>>,
}

impl Runtime {}

impl CtlServer for Runtime {}

impl esb::Handler<ServiceBus> for Runtime {
    type Request = Request;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        self.identity.clone()
    }

    fn handle(
        &mut self,
        endpoints: &mut Endpoints,
        bus: ServiceBus,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Self::Error> {
        match bus {
            ServiceBus::Msg => self.handle_rpc_msg(endpoints, source, request),
            ServiceBus::Ctl => self.handle_rpc_ctl(endpoints, source, request),
            _ => Err(Error::NotSupported(ServiceBus::Bridge, request.get_type())),
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
    fn handle_rpc_msg(
        &mut self,
        _endpoints: &mut Endpoints,
        _source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match request {
            Request::Hello => {
                // Ignoring; this is used to set remote identity at ZMQ level
            }

            req => {
                error!(
                    "MSG RPC can only be used for forwarding farcaster protocol messages, found {:?}, {:#?}",
                    req.get_type(), req
                )
            }
        }
        Ok(())
    }

    fn handle_rpc_ctl(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match request {
            Request::Hello => match &source {
                source => {
                    debug!("Received Hello from {}", source);
                }
            },

            Request::CheckpointMultipartChunk(checkpoint_multipart_chunk) => {
                if let Some(checkpoint_request) = checkpoint_handle_multipart_receive(
                    checkpoint_multipart_chunk,
                    &mut self.pending_checkpoint_chunks,
                )? {
                    self.handle_rpc_ctl(endpoints, source, checkpoint_request)?;
                }
            }
            Request::Checkpoint(Checkpoint { swap_id, state }) => {
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

            Request::RestoreCheckpoint(swap_id) => {
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

            Request::RetrieveAllCheckpointInfo => {
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
                    ServiceBus::Ctl,
                    source,
                    ServiceId::Farcasterd,
                    Request::CheckpointList(checkpointed_pub_offers),
                )?;
            }

            Request::RemoveCheckpoint(swap_id) => {
                self.database.delete_checkpoint_state(CheckpointKey {
                    swap_id,
                    service_id: ServiceId::Wallet,
                })?;
                self.database.delete_checkpoint_state(CheckpointKey {
                    swap_id,
                    service_id: ServiceId::Swap(swap_id),
                })?;
            }

            Request::AddressSecretKey(request::AddressSecretKey {
                address,
                secret_key,
            }) => {
                self.database.set_address(
                    &address,
                    &SecretKey::from_slice(&secret_key)
                        .expect("secret key is not valid secp256k1 secret key"),
                )?;
            }

            Request::GetAddressSecretKey(address) => {
                match self.database.get_address_secret_key(&address) {
                    Err(_) => endpoints.send_to(
                        ServiceBus::Ctl,
                        ServiceId::Database,
                        source,
                        Request::Failure(Failure {
                            code: 1,
                            info: format!("Could not retrieve secret key for address {}", address),
                        }),
                    )?,
                    Ok(secret_key) => {
                        endpoints.send_to(
                            ServiceBus::Ctl,
                            ServiceId::Database,
                            source,
                            Request::AddressSecretKey(request::AddressSecretKey {
                                address,
                                secret_key: secret_key.secret_bytes(),
                            }),
                        )?;
                    }
                }
            }

            Request::GetAddresses => {
                let addresses = self.database.get_all_addresses()?;
                endpoints.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Database,
                    source,
                    Request::AddressList(addresses.into()),
                )?;
            }

            Request::SetOfferStatus(OfferStatusPair { offer, status }) => {
                self.database.set_offer_status(&offer, &status)?;
            }

            Request::ListOffers(selector) => {
                let offer_status_pairs = self.database.get_offers(selector)?;
                endpoints.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Database,
                    source,
                    Request::OfferStatusList(offer_status_pairs.into()),
                )?;
            }

            _ => {
                error!("Request {} is not supported by the CTL interface", request);
            }
        }
        Ok(())
    }
}

pub fn checkpoint_handle_multipart_receive(
    checkpoint_multipart_chunk: request::CheckpointMultipartChunk,
    pending_checkpoint_chunks: &mut HashMap<[u8; 20], HashSet<CheckpointChunk>>,
) -> Result<Option<request::Request>, Error> {
    let request::CheckpointMultipartChunk {
        checksum,
        msg_index,
        msgs_total,
        serialized_state_chunk,
        swap_id,
    } = checkpoint_multipart_chunk;
    debug!("received checkpoint multipart message");
    if pending_checkpoint_chunks.contains_key(&checksum) {
        let chunks = pending_checkpoint_chunks
            .get_mut(&checksum)
            .expect("checked with contains_key");
        chunks.insert(CheckpointChunk {
            msg_index,
            serialized_state_chunk,
        });
    } else {
        let mut chunk = HashSet::new();
        chunk.insert(CheckpointChunk {
            msg_index,
            serialized_state_chunk,
        });
        pending_checkpoint_chunks.insert(checksum, chunk);
    }
    let mut chunks = pending_checkpoint_chunks
        .get(&checksum)
        .unwrap_or(&HashSet::new())
        .clone();
    if chunks.len() >= msgs_total {
        let mut chunk_tup_vec = chunks
            .drain()
            .map(|chunk| (chunk.msg_index, chunk.serialized_state_chunk))
            .collect::<Vec<(usize, Vec<u8>)>>(); // map the hashset to a vec for sorting
        chunk_tup_vec
            .sort_by(|(msg_number_a, _), (msg_number_b, _)| msg_number_a.cmp(&msg_number_b)); // sort in ascending order
        let chunk_vec = chunk_tup_vec
            .drain(..)
            .map(|(_, chunk)| chunk)
            .collect::<Vec<Vec<u8>>>(); // drop the extra integer index
        let serialized_checkpoint = chunk_vec.into_iter().flatten().collect::<Vec<u8>>(); // collect the chunked messages into a single serialized message
        if ripemd160::Hash::hash(&serialized_checkpoint).into_inner() != checksum {
            // this should never happen
            return Err(Error::Farcaster(
                "Unable to checkpoint the message, checksum did not match".to_string(),
            ));
        }
        // serialize request and return it
        let request = Request::Checkpoint(request::Checkpoint {
            swap_id,
            state: request::CheckpointState::strict_decode(std::io::Cursor::new(
                serialized_checkpoint,
            ))
            .map_err(|err| Error::Farcaster(err.to_string()))?,
        });
        Ok(Some(request))
    } else {
        Ok(None)
    }
}

pub fn checkpoint_send(
    endpoints: &mut Endpoints,
    swap_id: SwapId,
    source: ServiceId,
    destination: ServiceId,
    state: CheckpointState,
) -> Result<(), Error> {
    let mut serialized_state = vec![];
    let size = state
        .strict_encode(&mut serialized_state)
        .expect("strict encode of a checkpoint should not fail");

    // if the size exceeds a boundary, send a multi-part message
    let max_chunk_size = internet2::transport::MAX_FRAME_SIZE - 1024;
    debug!("checkpointing wallet state");
    if size > max_chunk_size {
        let checksum: [u8; 20] = ripemd160::Hash::hash(&serialized_state).into_inner();
        debug!("need to chunk the checkpoint message");
        let chunks: Vec<(usize, Vec<u8>)> = serialized_state
            .chunks_mut(max_chunk_size)
            .enumerate()
            .map(|(n, chunk)| (n, chunk.to_vec()))
            .collect();
        let chunks_total = chunks.len();
        for (n, chunk) in chunks {
            debug!(
                "sending chunked checkpoint message {} of a total {}",
                n + 1,
                chunks_total
            );
            endpoints.send_to(
                ServiceBus::Ctl,
                source.clone(),
                destination.clone(),
                Request::CheckpointMultipartChunk(CheckpointMultipartChunk {
                    checksum,
                    msg_index: n,
                    msgs_total: chunks_total,
                    serialized_state_chunk: chunk,
                    swap_id,
                }),
            )?;
        }
    } else {
        endpoints.send_to(
            ServiceBus::Ctl,
            source,
            destination,
            Request::Checkpoint(Checkpoint { swap_id, state }),
        )?;
    }
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
const LMDB_ADDRESSES: &str = "addresses";
const LMDB_OFFER_HISTORY: &str = "offer_history";

impl Database {
    fn new(path: PathBuf) -> Result<Database, lmdb::Error> {
        let env = lmdb::Environment::new()
            .set_map_size(10485760 * 1024 * 64)
            .set_max_dbs(3)
            .open(&path)?;
        env.create_db(Some(LMDB_CHECKPOINTS), lmdb::DatabaseFlags::empty())?;
        env.create_db(Some(LMDB_ADDRESSES), lmdb::DatabaseFlags::empty())?;
        env.create_db(Some(LMDB_OFFER_HISTORY), lmdb::DatabaseFlags::empty())?;
        Ok(Database(env))
    }

    fn set_offer_status(
        &mut self,
        offer: &PublicOffer<BtcXmr>,
        status: &OfferStatus,
    ) -> Result<(), lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_OFFER_HISTORY))?;
        let mut tx = self.0.begin_rw_txn()?;
        let mut key = vec![];
        let _key_size = offer.strict_encode(&mut key);
        if !tx.get(db, &key).is_err() {
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
                let offer =
                    PublicOffer::<BtcXmr>::strict_decode(std::io::Cursor::new(key.to_vec()))
                        .ok()?;
                Some(OfferStatusPair {
                    offer,
                    status: filtered_status,
                })
            })
            .collect();
        Ok(res)
    }

    fn set_address(
        &mut self,
        address: &bitcoin::Address,
        secret_key: &SecretKey,
    ) -> Result<(), lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_ADDRESSES))?;
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

    fn get_address_secret_key(
        &mut self,
        address: &bitcoin::Address,
    ) -> Result<SecretKey, lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_ADDRESSES))?;
        let tx = self.0.begin_ro_txn()?;
        let mut key = vec![];
        let _key_size = address.strict_encode(&mut key);
        let val = SecretKey::from_slice(tx.get(db, &key)?)
            .expect("we only insert private keys, so retrieving one should not fail");
        tx.abort();
        Ok(val)
    }

    fn get_all_addresses(&mut self) -> Result<Vec<bitcoin::Address>, lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_ADDRESSES))?;
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

    fn set_checkpoint_state(&mut self, key: &CheckpointKey, val: &[u8]) -> Result<(), lmdb::Error> {
        let db = self.0.open_db(Some(LMDB_CHECKPOINTS))?;
        let mut tx = self.0.begin_rw_txn()?;
        if !tx.get(db, &Vec::from(key.clone())).is_err() {
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
        if let Err(err) = tx.del(db, &Vec::from(key), None) {
            error!("{}", err);
        }
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
    database.set_address(&addr, &sk).unwrap();
    let val_retrieved = database.get_address_secret_key(&addr).unwrap();
    assert_eq!(sk, val_retrieved);
    let addrs = database.get_all_addresses().unwrap();
    assert!(addrs.contains(&addr));

    let offer_1 = PublicOffer::<BtcXmr>::from_str("Offer:Cke4ftrP5A71LQM2fvVdFMNR4gmBqNCsR11111uMFuZTAsNgpdK8DiK11111TB9zym113GTvtvqfD1111114A4TUGURtskxM3BUGLBGAdFDhJQVMQmiPUsL5vSTKhyBKw3Lh11111111111111111111111111111111111111111AfZ113XRBuStRU5H").unwrap();
    let offer_2 = PublicOffer::<BtcXmr>::from_str("Offer:Cke4ftrP5A71LQM2fvVdFMNR4grq1wi1D11111uMFuZTAsNgpdK8DiK11111TB9zym113GTvtvqfD1111114A4TUGURtskxM3BUGLBGAdFDhJQVMQmiPUsL5vSTKhyBKw3Lh11111111111111111111111111111111111111111AfZ113W5EEpvY61v").unwrap();

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
        .set_offer_status(&offer_1, &OfferStatus::Ended(request::Outcome::Buy))
        .unwrap();
    let offers_retrieved = database.get_offers(OfferStatusSelector::Ended).unwrap();
    assert_eq!(offer_1, offers_retrieved[0].offer);

    database
        .set_offer_status(&offer_2, &OfferStatus::Open)
        .unwrap();
    let offers_retrieved = database.get_offers(OfferStatusSelector::All).unwrap();
    let status_1 = OfferStatusPair {
        offer: offer_1,
        status: OfferStatus::Ended(request::Outcome::Buy),
    };
    let status_2 = OfferStatusPair {
        offer: offer_2,
        status: OfferStatus::Open,
    };
    assert!(offers_retrieved.len() == 2);
    assert!(offers_retrieved.contains(&status_1));
    assert!(offers_retrieved.contains(&status_2));
}
