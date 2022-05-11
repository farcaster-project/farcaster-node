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
use crate::LogStyle;
use crate::Senders;
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
use bitcoin::{
    hashes::hex::FromHex,
    secp256k1::{self, rand::thread_rng, PublicKey, Secp256k1, SecretKey, Signature},
    util::{
        bip32::{DerivationPath, ExtendedPrivKey},
        psbt::serialize::Deserialize,
    },
    Address,
};
use colored::Colorize;
use farcaster_core::{
    bitcoin::{
        segwitv0::{BuyTx, CancelTx, FundingTx, PunishTx, RefundTx},
        segwitv0::{LockTx, SegwitV0},
        Bitcoin, BitcoinSegwitV0,
    },
    blockchain::FeePriority,
    bundle::{
        AliceParameters, BobParameters, CoreArbitratingTransactions, FullySignedBuy,
        FullySignedPunish, FullySignedRefund, FundingTransaction, Proof, SignedAdaptorBuy,
        SignedAdaptorRefund, SignedArbitratingLock,
    },
    consensus::{self, CanonicalBytes, Decodable, Encodable},
    crypto::{ArbitratingKeyId, GenerateKey, SharedKeyId},
    crypto::{CommitmentEngine, ProveCrossGroupDleq},
    monero::{Monero, SHARED_VIEW_KEY_ID},
    negotiation::PublicOffer,
    protocol_message::{
        BuyProcedureSignature, CommitAliceParameters, CommitBobParameters, CoreArbitratingSetup,
        RefundProcedureSignatures,
    },
    role::{Alice, Bob, SwapRole, TradeRole},
    swap::btcxmr::{BtcXmr, KeyManager},
    swap::SwapId,
    syncer::{AddressTransaction, Boolean, Event},
    transaction::{Broadcastable, Fundable, Transaction, TxLabel, Witnessable},
};
use internet2::{LocalNode, ToNodeAddr, TypedEnum, LIGHTNING_P2P_DEFAULT_PORT};
// use lnp::{ChannelId as SwapId, TempChannelId as TempSwapId};
use microservices::esb::{self, Handler};
use request::{LaunchSwap, NodeId};

pub fn run(
    config: ServiceConfig,
    data_dir: PathBuf,
) -> Result<(), Error> {
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

impl Runtime {
}

impl CtlServer for Runtime {}

impl esb::Handler<ServiceBus> for Runtime {
    type Request = Request;
    type Address = ServiceId;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        self.identity.clone()
    }

    fn handle(
        &mut self,
        senders: &mut esb::SenderList<ServiceBus, ServiceId>,
        bus: ServiceBus,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Self::Error> {
        match bus {
            ServiceBus::Msg => self.handle_rpc_msg(senders, source, request),
            ServiceBus::Ctl => self.handle_rpc_ctl(senders, source, request),
            _ => Err(Error::NotSupported(ServiceBus::Bridge, request.get_type())),
        }
    }

    fn handle_err(&mut self, _: esb::Error) -> Result<(), esb::Error> {
        // We do nothing and do not propagate error; it's already being reported
        // with `error!` macro by the controller. If we propagate error here
        // this will make whole daemon panic
        Ok(())
    }
}

impl Runtime {
    fn send_farcasterd(
        &self,
        senders: &mut Senders,
        message: request::Request,
    ) -> Result<(), Error> {
        senders.send_to(
            ServiceBus::Ctl,
            self.identity(),
            ServiceId::Farcasterd,
            message,
        )?;
        Ok(())
    }

    fn handle_rpc_msg(
        &mut self,
        senders: &mut Senders,
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
        senders: &mut Senders,
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
                //             self.send_ctl(senders, source, request)?
                //         }
                //     }
                // }
                source => {
                    debug!("Received Hello from {}", source);
                }
            },

            Request::Checkpoint(CheckpointWalletAlicePreBuy) => {
                todo!();
            }

            Request::Checkpoint(CheckpointWalletAlicePreBuy) => {
                todo!();
            }

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

struct CheckpointGetSet(lmdb::Environment);

impl CheckpointGetSet {
    fn new(path: PathBuf) -> CheckpointGetSet {
        let env = lmdb::Environment::new().open(&path).unwrap();
        env.create_db(None, lmdb::DatabaseFlags::empty()).unwrap();
        CheckpointGetSet(env)
    }

    fn set_state(&mut self, key: &SwapId, val: &[u8]) {
        let db = self.0.open_db(None).unwrap();
        let mut tx = self.0.begin_rw_txn().unwrap();
        if !tx.get(db, &key).is_err() {
            tx.del(db, &key, None).unwrap();
        }
        tx.put(db, &key, &val, lmdb::WriteFlags::empty()).unwrap();
        tx.commit().unwrap();
    }

    fn get_state(&mut self, key: &SwapId) -> Vec<u8> {
        let db = self.0.open_db(None).unwrap();
        let tx = self.0.begin_ro_txn().unwrap();
        let val: Vec<u8> = tx.get(db, &key).unwrap().into();
        tx.abort();
        val
    }
}

#[test]
fn test_lmdb_state() {
    let val1 = vec![0, 1];
    let val2 = vec![2, 3, 4, 5];
    let key1 = SwapId::random();
    let key2 = SwapId::random();
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
