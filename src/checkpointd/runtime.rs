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

use crate::swapd::get_swap_id;
use crate::walletd::NodeSecrets;
use crate::Endpoints;
use crate::LogStyle;
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
        identity: ServiceId::Wallet,
        checkpoints: CheckpointGetSet::new(data_dir),
    };

    Service::run(config, runtime, false)
}

pub struct Runtime {
    identity: ServiceId,
    checkpoints: CheckpointGetSet,
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
    fn send_farcasterd(
        &self,
        endpoints: &mut Endpoints,
        message: request::Request,
    ) -> Result<(), Error> {
        endpoints.send_to(
            ServiceBus::Ctl,
            self.identity(),
            ServiceId::Farcasterd,
            message,
        )?;
        Ok(())
    }

    fn handle_rpc_msg(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        let req_swap_id = get_swap_id(&source).ok();
        match &request {
            Request::Protocol(Msg::TakerCommit(_)) if source == ServiceId::Farcasterd => {}
            Request::Protocol(msg)
                if req_swap_id.is_some() && Some(msg.swap_id()) != req_swap_id =>
            {
                error!("Msg and source don't have same swap_id, ignoring...");
                return Ok(());
            }
            // TODO enter farcasterd messages allowed
            _ => {}
        }
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
                // ServiceId::Swap(swap_id) => {
                //     if let Some(option_req) = self.swaps.get_mut(swap_id) {
                //         trace!("Known swapd, you launched it");
                //         if let Some(req) = option_req {
                //             let request = req.clone();
                //             *option_req = None;
                //             self.send_ctl(endpoints, source, request)?
                //         }
                //     }
                // }
                source => {
                    debug!("Received Hello from {}", source);
                }
            },

            // TODO: use RFC 2363 once stabilized: https://github.com/rust-lang/rust/issues/60553
            // then, instead of explicitly matching on checkpoint variant, can use index as with fieldless enums, so have one generic request match for all originating services & checkpoint indices
            Request::Checkpoint(request::Checkpoint::CheckpointWalletAlicePreBuy(wallet_state)) => {
                let key = (get_swap_id(&source)?, self.identity.clone()).into();
                let mut state_encoded = vec![];
                let _state_size = wallet_state.consensus_encode(&mut state_encoded);
                self.checkpoints.set_state(&key, &state_encoded);
            }

            // Request::Checkpoint(request::Checkpoint::CheckpointSwapAlicePreBuy(_)) => {
            //     todo!();
            // }
            _ => {
                error!(
                    "Request {:?} is not supported by the CTL interface",
                    request
                );
            }
        }
        Ok(())
    }
}

// TODO: replace this ugly temp structure
struct CheckpointKey(Vec<u8>);

impl From<(SwapId, ServiceId)> for CheckpointKey {
    fn from((swapid, serviceid): (SwapId, ServiceId)) -> Self {
        CheckpointKey(
            Into::<[u8; 32]>::into(swapid)
                .iter()
                .cloned()
                .chain(Into::<Vec<u8>>::into(serviceid).iter().cloned())
                .collect(),
        )
    }
}

struct CheckpointGetSet(lmdb::Environment);

impl CheckpointGetSet {
    fn new(path: PathBuf) -> CheckpointGetSet {
        let env = lmdb::Environment::new().open(&path).unwrap();
        env.create_db(None, lmdb::DatabaseFlags::empty()).unwrap();
        CheckpointGetSet(env)
    }

    fn set_state(&mut self, key: &CheckpointKey, val: &[u8]) {
        let db = self.0.open_db(None).unwrap();
        let mut tx = self.0.begin_rw_txn().unwrap();
        if !tx.get(db, &key.0).is_err() {
            tx.del(db, &key.0, None).unwrap();
        }
        tx.put(db, &key.0, &val, lmdb::WriteFlags::empty()).unwrap();
        tx.commit().unwrap();
    }

    fn get_state(&mut self, key: &CheckpointKey) -> Vec<u8> {
        let db = self.0.open_db(None).unwrap();
        let tx = self.0.begin_ro_txn().unwrap();
        let val: Vec<u8> = tx.get(db, &key.0).unwrap().into();
        tx.abort();
        val
    }
}

#[test]
fn test_lmdb_state() {
    let val1 = vec![0, 1];
    let val2 = vec![2, 3, 4, 5];
    let key1 = (SwapId::random(), ServiceId::Checkpoint).into();
    let key2 = (SwapId::random(), ServiceId::Checkpoint).into();
    let path = std::env::current_dir().unwrap();
    let mut state = CheckpointGetSet::new(path.to_path_buf());
    state.set_state(&key1, &val1);
    let res = state.get_state(&key1);
    assert_eq!(val1, res);
    state.set_state(&key1, &val2);
    let res = state.get_state(&key1);
    assert_eq!(val2, res);
    state.set_state(&key2, &val2);
    let res = state.get_state(&key2);
    assert_eq!(val2, res);
}
