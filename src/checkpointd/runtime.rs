use crate::farcaster_core::consensus::Encodable;
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
            self, BitcoinAddress, Commit, Keys, MoneroAddress, Msg, Params, Reveal, Token, Tx,
        },
        Request, ServiceBus,
    },
    syncerd::SweepXmrAddress,
};
use crate::{CtlServer, Error, Service, ServiceConfig, ServiceId};
use colored::Colorize;
use internet2::TypedEnum;
use microservices::esb::{self, Handler};
use request::{LaunchSwap, NodeId};

pub fn run(config: ServiceConfig, data_dir: PathBuf) -> Result<(), Error> {
    let runtime = Runtime {
        identity: ServiceId::Checkpoint,
        checkpoints: CheckpointGetSet::new(data_dir).unwrap(),
        pending_checkpoint_chunks: map![],
    };

    Service::run(config, runtime, false)
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct CheckpointChunk {
    pub msg_index: usize,
    pub serialized_state_chunk: Vec<u8>,
}

pub struct Runtime {
    identity: ServiceId,
    checkpoints: CheckpointGetSet,
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

            Request::CheckpointMultipartChunk(request::CheckpointMultipartChunk {
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
                    let request = Request::Checkpoint(request::Checkpoint {
                        swap_id,
                        state: request::CheckpointState::strict_decode(std::io::Cursor::new(
                            serialized_checkpoint,
                        ))
                        .map_err(|err| Error::Farcaster(err.to_string()))?,
                    });
                    self.handle_rpc_ctl(endpoints, source, request)?;
                }
            }

            Request::Checkpoint(request::Checkpoint { swap_id, state }) => {
                debug!("setting checkpoint");
                let key = CheckpointKey {
                    swap_id,
                    service_id: source,
                };
                let mut state_encoded = vec![];
                let _state_size = state.strict_encode(&mut state_encoded);
                self.checkpoints
                    .set_state(&key, &state_encoded)
                    .expect("failed to set checkpoint state");
            }

            Request::RetrieveAllCheckpointInfo => {
                let _pairs = self.checkpoints.get_all_key_value_pairs();
            }

            Request::DeleteCheckpoint(swap_id) => {
                self.checkpoints
                    .delete_state(CheckpointKey {
                        swap_id,
                        service_id: ServiceId::Wallet,
                    })
                    .expect("failed to delete Wallet state");
                self.checkpoints
                    .delete_state(CheckpointKey {
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

struct CheckpointGetSet(lmdb::Environment);

impl CheckpointGetSet {
    fn new(path: PathBuf) -> Result<CheckpointGetSet, lmdb::Error> {
        let env = lmdb::Environment::new().open(&path)?;
        env.create_db(None, lmdb::DatabaseFlags::empty())?;
        Ok(CheckpointGetSet(env))
    }

    fn set_state(&mut self, key: &CheckpointKey, val: &[u8]) -> Result<(), lmdb::Error> {
        let db = self.0.open_db(None)?;
        let mut tx = self.0.begin_rw_txn()?;
        if !tx.get(db, &Vec::from(key.clone())).is_err() {
            tx.del(db, &Vec::from(key.clone()), None)?;
        }
        tx.put(db, &Vec::from(key.clone()), &val, lmdb::WriteFlags::empty())?;
        tx.commit()?;
        Ok(())
    }

    fn get_state(&mut self, key: &CheckpointKey) -> Result<Vec<u8>, lmdb::Error> {
        let db = self.0.open_db(None)?;
        let tx = self.0.begin_ro_txn()?;
        let val: Vec<u8> = tx.get(db, &Vec::from(key.clone()))?.into();
        tx.abort();
        Ok(val)
    }

    fn delete_state(&mut self, key: CheckpointKey) -> Result<(), lmdb::Error> {
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
        service_id: ServiceId::Checkpoint,
    };
    let path = std::env::current_dir().unwrap();
    let mut state = CheckpointGetSet::new(path.to_path_buf()).unwrap();
    state.set_state(&key1, &val1).unwrap();
    let res = state.get_state(&key1).unwrap();
    assert_eq!(val1, res);
    state.set_state(&key1, &val2).unwrap();
    let res = state.get_state(&key1).unwrap();
    assert_eq!(val2, res);
    state.set_state(&key2, &val2).unwrap();
    let res = state.get_state(&key2).unwrap();
    assert_eq!(val2, res);
    state.delete_state(key2.clone()).unwrap();
    let res = state.get_state(&key2);
    assert!(res.is_err());
    state.get_all_key_value_pairs().unwrap();
}
