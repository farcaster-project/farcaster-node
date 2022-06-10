use crate::farcaster_core::consensus::Encodable;
use farcaster_core::negotiation::PublicOffer;
use farcaster_core::swap::btcxmr::BtcXmr;
use farcaster_core::swap::SwapId;
use lmdb::{Cursor, Transaction as LMDBTransaction};
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

            Request::CheckpointMultipartChunk(CheckpointMultipartChunk {
                checksum,
                msg_index,
                msgs_total,
                serialized_state_chunk,
                swap_id,
            }) => {
                debug!("received checkpoint multipart message");
                if self.pending_checkpoint_chunks.contains_key(&checksum) {
                    let chunks = self
                        .pending_checkpoint_chunks
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
                    self.pending_checkpoint_chunks.insert(checksum, chunk);
                }
                let mut chunks = self
                    .pending_checkpoint_chunks
                    .get(&checksum)
                    .unwrap_or(&HashSet::new())
                    .clone();
                if chunks.len() >= msgs_total {
                    let mut chunk_tup_vec = chunks
                        .drain()
                        .map(|chunk| (chunk.msg_index, chunk.serialized_state_chunk))
                        .collect::<Vec<(usize, Vec<u8>)>>(); // map the hashset to a vec for sorting
                    chunk_tup_vec.sort_by(|(msg_number_a, _), (msg_number_b, _)| {
                        msg_number_a.cmp(&msg_number_b)
                    }); // sort in ascending order
                    let chunk_vec = chunk_tup_vec
                        .drain(..)
                        .map(|(_, chunk)| chunk)
                        .collect::<Vec<Vec<u8>>>(); // drop the extra integer index
                    let serialized_checkpoint =
                        chunk_vec.into_iter().flatten().collect::<Vec<u8>>(); // collect the chunked messages into a single serialized message
                    if ripemd160::Hash::hash(&serialized_checkpoint).into_inner() != checksum {
                        // this should never happen
                        error!("Unable to checkpoint the message, checksum did not match");
                        return Ok(());
                    }
                    // serialize request and recurse to handle the actual request
                    let request = Request::Checkpoint(Checkpoint {
                        swap_id,
                        state: CheckpointState::strict_decode(std::io::Cursor::new(
                            serialized_checkpoint,
                        ))
                        .map_err(|err| Error::Farcaster(err.to_string()))?,
                    });
                    self.handle_rpc_ctl(endpoints, source, request)?;
                }
            }

            Request::Checkpoint(Checkpoint { swap_id, state }) => {
                match state {
                    CheckpointState::CheckpointWalletAlice(_)
                    | CheckpointState::CheckpointWalletBob(_) => {
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
                self.database
                    .set_checkpoint_state(&key, &state_encoded)
                    .expect("failed to set checkpoint state");
            }

            Request::RestoreCheckpoint(swap_id) => {
                match self.database.get_checkpoint_state(&CheckpointKey {
                    swap_id,
                    service_id: ServiceId::Wallet,
                }) {
                    Ok(raw_state) => {
                        match CheckpointState::strict_decode(std::io::Cursor::new(raw_state)) {
                            Ok(CheckpointState::CheckpointWalletAlice(state)) => {
                                checkpoint_restore(
                                    endpoints,
                                    swap_id,
                                    ServiceId::Wallet,
                                    CheckpointState::CheckpointWalletAlice(state),
                                )?;
                            }
                            Ok(CheckpointState::CheckpointWalletBob(state)) => {
                                checkpoint_restore(
                                    endpoints,
                                    swap_id,
                                    ServiceId::Wallet,
                                    CheckpointState::CheckpointWalletBob(state),
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
                match self.checkpoints.get_state(&CheckpointKey {
                    swap_id,
                    service_id: ServiceId::Swap(swap_id),
                }) {
                    Ok(raw_state) => {
                        match CheckpointState::strict_decode(std::io::Cursor::new(raw_state)) {
                            Ok(CheckpointState::CheckpointSwapd(state)) => {
                                checkpoint_restore(
                                    endpoints,
                                    swap_id,
                                    ServiceId::Swap(swap_id),
                                    CheckpointState::CheckpointSwapd(state),
                                )?;
                            }
                            Ok(CheckpointState::CheckpointWalletAlice(_))
                            | Ok(CheckpointState::CheckpointWalletBob(_)) => {
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
                let pairs = self
                    .database
                    .get_all_key_value_pairs()
                    .expect("unable retrieve all checkpointed key-value pairs");
                let checkpointed_pub_offers: List<CheckpointEntry> = pairs
                    .iter()
                    .filter_map(|(checkpoint_key, state)| {
                        let state =
                            CheckpointState::strict_decode(std::io::Cursor::new(state)).ok()?;
                        match checkpoint_key.service_id {
                            ServiceId::Wallet => match state {
                                CheckpointState::CheckpointWalletAlice(checkpoint) => {
                                    Some(CheckpointEntry {
                                        swap_id: checkpoint_key.swap_id,
                                        public_offer: checkpoint.pub_offer,
                                        trade_role: checkpoint.local_trade_role,
                                    })
                                }
                                CheckpointState::CheckpointWalletBob(checkpoint) => {
                                    Some(CheckpointEntry {
                                        swap_id: checkpoint_key.swap_id,
                                        public_offer: checkpoint.pub_offer,
                                        trade_role: checkpoint.local_trade_role,
                                    })
                                }
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
                self.database
                    .delete_checkpoint_state(CheckpointKey {
                        swap_id,
                        service_id: ServiceId::Wallet,
                    })
                    .expect("failed to delete Wallet state");
                self.database
                    .delete_checkpoint_state(CheckpointKey {
                        swap_id,
                        service_id: ServiceId::Swap(swap_id),
                    })
                    .expect("failed to delete Swap state");
            }

            _ => {
                error!("Request {} is not supported by the CTL interface", request);
            }
        }
        Ok(())
    }
}

pub fn checkpoint_restore(
    endpoints: &mut Endpoints,
    swap_id: SwapId,
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
                ServiceId::Database,
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
            ServiceId::Checkpoint,
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

impl Database {
    fn new(path: PathBuf) -> Result<Database, lmdb::Error> {
        let env = lmdb::Environment::new().open(&path)?;
        env.create_db(None, lmdb::DatabaseFlags::empty())?;
        Ok(Database(env))
    }

    fn set_checkpoint_state(&mut self, key: &CheckpointKey, val: &[u8]) -> Result<(), lmdb::Error> {
        let db = self.0.open_db(None)?;
        let mut tx = self.0.begin_rw_txn()?;
        if !tx.get(db, &Vec::from(key.clone())).is_err() {
            tx.del(db, &Vec::from(key.clone()), None)?;
        }
        tx.put(db, &Vec::from(key.clone()), &val, lmdb::WriteFlags::empty())?;
        tx.commit()?;
        Ok(())
    }

    fn get_checkpoint_state(&mut self, key: &CheckpointKey) -> Result<Vec<u8>, lmdb::Error> {
        let db = self.0.open_db(None)?;
        let tx = self.0.begin_ro_txn()?;
        let val: Vec<u8> = tx.get(db, &Vec::from(key.clone()))?.into();
        tx.abort();
        Ok(val)
    }

    fn delete_checkpoint_state(&mut self, key: CheckpointKey) -> Result<(), lmdb::Error> {
        let db = self.0.open_db(None)?;
        let mut tx = self.0.begin_rw_txn()?;
        if let Err(err) = tx.del(db, &Vec::from(key), None) {
            error!("{}", err);
        }
        tx.commit()?;
        Ok(())
    }

    fn get_all_key_value_pairs(&mut self) -> Result<Vec<(CheckpointKey, Vec<u8>)>, lmdb::Error> {
        let db = self.0.open_db(None)?;
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
    database.get_all_key_value_pairs().unwrap();
}
