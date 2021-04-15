// LNP Node: node running lightning network protocol and generalized lightning
// channels.
// Written in 2020 by
//     Dr. Maxim Orlovsky <orlovsky@pandoracore.com>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the MIT License
// along with this software.
// If not, see <https://opensource.org/licenses/MIT>.

use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::time::{Duration, SystemTime};

use bitcoin::hashes::{sha256, Hash, HashEngine};
use bitcoin::secp256k1;
use bitcoin::util::bip143::SigHashCache;
use bitcoin::{OutPoint, SigHashType, Transaction};
use internet2::zmqsocket::{self, ZmqSocketAddr, ZmqType};
use internet2::{
    session, CreateUnmarshaller, LocalNode, NodeAddr, Session, TypedEnum,
    Unmarshall, Unmarshaller,
};
use lnp::payment::bolt3::{ScriptGenerators, TxGenerators};
use lnp::payment::htlc::{HtlcKnown, HtlcSecret};
use lnp::payment::{self, AssetsBalance, Lifecycle};
use lnp::{
    message, ChannelId as SwapId, Messages, TempChannelId as TempSwapId,
};
use lnpbp::{chain::AssetId, Chain};
use microservices::esb::{self, Handler};
use wallet::{HashPreimage, PubkeyScript};

use super::storage::{self, Driver};
use crate::rpc::request::SwapInfo;
use crate::rpc::{request, Request, ServiceBus};
use crate::{Config, CtlServer, Error, LogStyle, Senders, Service, ServiceId};

pub fn run(
    config: Config,
    local_node: LocalNode,
    channel_id: SwapId,
    chain: Chain,
) -> Result<(), Error> {
    let runtime = Runtime {
        identity: ServiceId::Swap(channel_id),
        peer_service: ServiceId::Loopback,
        local_node,
        chain,
        channel_id: zero!(),
        temporary_channel_id: channel_id.into(),
        state: default!(),
        local_capacity: 0,
        remote_capacity: 0,
        local_balances: zero!(),
        remote_balances: zero!(),
        funding_outpoint: default!(),
        remote_peer: None,
        started: SystemTime::now(),
        commitment_number: 0,
        total_payments: 0,
        pending_payments: 0,
        params: default!(),
        local_keys: dumb!(),
        remote_keys: dumb!(),
        offered_htlc: empty!(),
        received_htlc: empty!(),
        is_originator: false,
        obscuring_factor: 0,
        enquirer: None,
        storage: Box::new(storage::DiskDriver::init(
            channel_id,
            Box::new(storage::DiskConfig {
                path: Default::default(),
            }),
        )?),
    };

    Service::run(config, runtime, false)
}

trait AssetUnit {}

use farcaster_chains::bitcoin::{Amount, CSVTimelock, fee::SatPerVByte};

pub enum AliceLifecycle{
    StartA,
    CommitA,
    RevealA,
    RefundProcedureSignatures,
    FinishA,
}

// enum BInterdaemon {
//     StartB(),
//     CommitB(BCommit, OKBCommit),
//     RevealB(ACommitB, BCommitA, BReveal),
//     CorearbB(BRevealA, ARevealB, OKCoreArb, CoreArbit),
//     BuyProcSigB(CoreArbitA, RfndProcSigB, OKbuy),
//     FinishB(),
// }

pub struct RuntimeSwapd {
    identify: ServiceId,
    peer_service: ServiceId,
    local_node: LocalNode,
    swap_id: SwapId,
    temporary_swap_id: TempSwapId,
    funding_outpoint: OutPoint,
    accordant_amount: Amount,
    arbitrating_amount: u64,
    cancel_timelock: CSVTimelock,
    punish_timelock: CSVTimelock,
    swap_role: farcaster_core::role::SwapRole,
    fee_strategy: SatPerVByte,
    enquirer: Option<ServiceId>,
    #[allow(dead_code)]
    storage: Box<dyn storage::Driver>,
}


pub struct Runtime {
    identity: ServiceId,
    peer_service: ServiceId,
    local_node: LocalNode,
    chain: Chain,

    channel_id: SwapId,
    temporary_channel_id: TempSwapId,
    state: Lifecycle,
    local_capacity: u64,
    remote_capacity: u64,
    local_balances: AssetsBalance,
    remote_balances: AssetsBalance,
    funding_outpoint: OutPoint,
    remote_peer: Option<NodeAddr>,
    started: SystemTime,
    commitment_number: u64,
    total_payments: u64,
    pending_payments: u16,
    params: payment::channel::Params,
    local_keys: payment::channel::Keyset,
    remote_keys: payment::channel::Keyset,

    offered_htlc: Vec<HtlcKnown>,
    received_htlc: Vec<HtlcSecret>,

    is_originator: bool,
    obscuring_factor: u64,

    enquirer: Option<ServiceId>,
    #[allow(dead_code)]
    storage: Box<dyn storage::Driver>,
}

impl CtlServer for Runtime {}

impl Runtime {
    #[inline]
    pub fn node_id(&self) -> secp256k1::PublicKey {
        self.local_node.node_id()
    }

    #[inline]
    pub fn channel_capacity(&self) -> u64 {
        self.local_capacity + self.remote_capacity
    }
}

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
            _ => {
                Err(Error::NotSupported(ServiceBus::Bridge, request.get_type()))
            }
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
    fn send_peer(
        &self,
        senders: &mut Senders,
        message: Messages,
    ) -> Result<(), Error> {
        senders.send_to(
            ServiceBus::Msg,
            self.identity(),
            self.peer_service.clone(), // = ServiceId::Loopback
            Request::PeerMessage(message),
        )?;
        Ok(())
    }

    fn handle_rpc_msg(
        &mut self,
        senders: &mut Senders,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match request {
            Request::PeerMessage(Messages::AcceptChannel(accept_channel)) => {
                self.state = Lifecycle::Accepted;

                let enquirer = self.enquirer.clone();

                self.channel_accepted(senders, &accept_channel, &source)
                    .map_err(|err| {
                        self.report_failure_to(
                            senders,
                            &enquirer,
                            microservices::rpc::Failure {
                                code: 0, // TODO: Create error type system
                                info: err.to_string(),
                            },
                        )
                    })?;

                // Construct funding output scriptPubkey
                let remote_pk = accept_channel.funding_pubkey;
                let local_pk = self.local_keys.funding_pubkey;
                trace!(
                    "Generating script pubkey from local {} and remote {}",
                    local_pk,
                    remote_pk
                );
                let script_pubkey = PubkeyScript::ln_funding(
                    self.channel_capacity(),
                    local_pk,
                    remote_pk,
                );
                trace!("Funding script: {}", script_pubkey);
                if let Some(addr) = bitcoin::Network::try_from(&self.chain)
                    .ok()
                    .and_then(|network| script_pubkey.address(network))
                {
                    debug!("Funding address: {}", addr);
                } else {
                    error!(
                        "{} {}",
                        "Unable to generate funding address for the current network "
                            .err(),
                        self.chain.err()
                    )
                }

                // Ignoring possible error here: do not want to
                // halt the channel just because the client disconnected
                let _ = self.send_ctl(
                    senders,
                    &enquirer,
                    Request::SwapFunding(script_pubkey),
                );
            }

            Request::PeerMessage(Messages::FundingCreated(funding_created)) => {
                let enquirer = self.enquirer.clone();

                self.state = Lifecycle::Funding;

                let funding_signed =
                    self.funding_created(senders, funding_created)?;

                self.send_peer(
                    senders,
                    Messages::FundingSigned(funding_signed),
                )?;

                self.state = Lifecycle::Funded;

                // Ignoring possible error here: do not want to
                // halt the channel just because the client disconnected
                let msg = format!(
                    "{} both signatures present",
                    "Channel funded:".ended()
                );
                info!("{}", msg);
                let _ = self.report_progress_to(senders, &enquirer, msg);
            }

            Request::PeerMessage(Messages::FundingSigned(_funding_signed)) => {
                // TODO:
                //      1. Get commitment tx
                //      2. Verify signature
                //      3. Save signature/commitment tx
                //      4. Send funding locked request

                let enquirer = self.enquirer.clone();

                self.state = Lifecycle::Funded;

                // Ignoring possible error here: do not want to
                // halt the channel just because the client disconnected
                let msg = format!(
                    "{} both signatures present",
                    "Channel funded:".ended()
                );
                info!("{}", msg);
                let _ = self.report_progress_to(senders, &enquirer, msg);

                let funding_locked = message::FundingLocked {
                    channel_id: self.channel_id,
                    next_per_commitment_point: self
                        .local_keys
                        .first_per_commitment_point,
                };

                self.send_peer(
                    senders,
                    Messages::FundingLocked(funding_locked),
                )?;

                self.state = Lifecycle::Active;
                self.local_capacity = self.params.funding_satoshis;

                // Ignoring possible error here: do not want to
                // halt the channel just because the client disconnected
                let msg = format!(
                    "{} transaction confirmed",
                    "Channel active:".ended()
                );
                info!("{}", msg);
                let _ = self.report_success_to(senders, &enquirer, Some(msg));
            }

            Request::PeerMessage(Messages::FundingLocked(_funding_locked)) => {
                let enquirer = self.enquirer.clone();

                self.state = Lifecycle::Locked;

                // TODO:
                //      1. Change the channel state
                //      2. Do something with per-commitment point

                self.state = Lifecycle::Active;
                self.remote_capacity = self.params.funding_satoshis;

                // Ignoring possible error here: do not want to
                // halt the channel just because the client disconnected
                let msg = format!(
                    "{} transaction confirmed",
                    "Channel active:".ended()
                );
                info!("{}", msg);
                let _ = self.report_success_to(senders, &enquirer, Some(msg));
            }

            Request::PeerMessage(Messages::UpdateAddHtlc(update_add_htlc)) => {
                let _commitment_signed =
                    self.htlc_receive(senders, update_add_htlc)?;
            }

            Request::PeerMessage(Messages::CommitmentSigned(
                _commitment_signed,
            )) => {}

            Request::PeerMessage(Messages::RevokeAndAck(_revoke_ack)) => {}

            Request::PeerMessage(_) => {
                // Ignore the rest of LN peer messages
            }

            _ => {
                error!(
                    "MSG RPC can be only used for forwarding LNPWP messages"
                );
                return Err(Error::NotSupported(
                    ServiceBus::Msg,
                    request.get_type(),
                ));
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
            Request::OpenSwapWith(request::CreateSwap {
                swap_req: channel_req,
                peerd,
                report_to,
            }) => {
                self.peer_service = peerd.clone();
                self.enquirer = report_to.clone();

                if let ServiceId::Peer(ref addr) = peerd {
                    self.remote_peer = Some(addr.clone());
                }

                self.open_channel(senders, &channel_req).map_err(|err| {
                    self.report_failure_to(
                        senders,
                        &report_to,
                        microservices::rpc::Failure {
                            code: 0, // TODO: Create error type system
                            info: err.to_string(),
                        },
                    )
                })?;

                self.send_peer(senders, Messages::OpenChannel(channel_req))?;

                self.state = Lifecycle::Proposed;
            }

            Request::AcceptSwapFrom(request::CreateSwap {
                swap_req: channel_req,
                peerd,
                report_to,
            }) => {
                self.peer_service = peerd.clone();
                self.state = Lifecycle::Proposed;

                if let ServiceId::Peer(ref addr) = peerd {
                    self.remote_peer = Some(addr.clone());
                }

                let accept_channel = self
                    .accept_channel(senders, &channel_req, &peerd)
                    .map_err(|err| {
                        self.report_failure_to(
                            senders,
                            &report_to,
                            microservices::rpc::Failure {
                                code: 0, // TODO: Create error type system
                                info: err.to_string(),
                            },
                        )
                    })?;

                self.send_peer(
                    senders,
                    Messages::AcceptChannel(accept_channel),
                )?;

                self.state = Lifecycle::Accepted;
            }

            Request::FundSwap(funding_outpoint) => {
                self.enquirer = source.into();

                let funding_created =
                    self.fund_channel(senders, funding_outpoint)?;

                self.state = Lifecycle::Funding;
                self.send_peer(
                    senders,
                    Messages::FundingCreated(funding_created),
                )?;
            }

            Request::GetInfo => {
                fn bmap<T>(
                    remote_peer: &Option<NodeAddr>,
                    v: &T,
                ) -> BTreeMap<NodeAddr, T>
                where
                    T: Clone,
                {
                    remote_peer
                        .as_ref()
                        .map(|p| bmap! { p.clone() => v.clone() })
                        .unwrap_or_default()
                }

                let channel_id = if self.channel_id == zero!() {
                    None
                } else {
                    Some(self.channel_id)
                };
                let info = SwapInfo {
                    channel_id,
                    temporary_channel_id: self.temporary_channel_id,
                    state: self.state,
                    local_capacity: self.local_capacity,
                    remote_capacities: bmap(
                        &self.remote_peer,
                        &self.remote_capacity,
                    ),
                    assets: none!(),
                    local_balances: self.local_balances.clone(),
                    remote_balances: bmap(
                        &self.remote_peer,
                        &self.remote_balances,
                    ),
                    funding_outpoint: self.funding_outpoint,
                    remote_peers: self
                        .remote_peer
                        .clone()
                        .map(|p| vec![p])
                        .unwrap_or_default(),
                    uptime: SystemTime::now()
                        .duration_since(self.started)
                        .unwrap_or(Duration::from_secs(0)),
                    since: self
                        .started
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or(Duration::from_secs(0))
                        .as_secs(),
                    commitment_updates: self.commitment_number,
                    total_payments: self.total_payments,
                    pending_payments: self.pending_payments,
                    is_originator: self.is_originator,
                    params: self.params,
                    local_keys: self.local_keys.clone(),
                    remote_keys: bmap(&self.remote_peer, &self.remote_keys),
                };
                self.send_ctl(senders, source, Request::SwapInfo(info))?;
            }

            _ => {
                error!("Request is not supported by the CTL interface");
                return Err(Error::NotSupported(
                    ServiceBus::Ctl,
                    request.get_type(),
                ));
            }
        }
        Ok(())
    }
}

impl Runtime {
    pub fn update_channel_id(
        &mut self,
        senders: &mut Senders,
    ) -> Result<(), Error> {
        let enquirer = self.enquirer.clone();

        // Update channel id!
        self.channel_id = SwapId::with(self.funding_outpoint);
        debug!("Updating channel id to {}", self.channel_id);
        self.send_ctl(
            senders,
            ServiceId::Farcasterd,
            Request::UpdateSwapId(self.channel_id),
        )?;
        self.send_ctl(
            senders,
            self.peer_service.clone(),
            Request::UpdateSwapId(self.channel_id),
        )?;
        // self.identity = self.channel_id.into();
        let msg = format!(
            "{} set to {}",
            "Channel ID".ended(),
            self.channel_id.ender()
        );
        info!("{}", msg);
        let _ = self.report_progress_to(senders, &enquirer, msg);

        Ok(())
    }

    pub fn open_channel(
        &mut self,
        senders: &mut Senders,
        channel_req: &message::OpenChannel,
    ) -> Result<(), payment::channel::NegotiationError> {
        info!(
            "{} remote peer to {} with temp id {:#}",
            "Proposing".promo(),
            "open a channel".promo(),
            channel_req.temporary_channel_id.promoter()
        );
        // Ignoring possible reporting errors here and after: do not want to
        // halt the channel just because the client disconnected
        let enquirer = self.enquirer.clone();
        let _ = self.report_progress_to(
            senders,
            &enquirer,
            format!("Proposing remote peer to open a channel"),
        );

        self.is_originator = true;
        self.params = payment::channel::Params::with(&channel_req)?;
        self.local_keys = payment::channel::Keyset::from(channel_req);

        Ok(())
    }

    pub fn accept_channel(
        &mut self,
        senders: &mut Senders,
        channel_req: &message::OpenChannel,
        peerd: &ServiceId,
    ) -> Result<message::AcceptChannel, payment::channel::NegotiationError>
    {
        let msg = format!(
            "{} with temp id {:#} from remote peer {}",
            "Accepting channel".promo(),
            channel_req.temporary_channel_id.promoter(),
            peerd.promoter()
        );
        info!("{}", msg);

        // Ignoring possible reporting errors here and after: do not want to
        // halt the channel just because the client disconnected
        let enquirer = self.enquirer.clone();
        let _ = self.report_progress_to(senders, &enquirer, msg);

        self.is_originator = false;
        self.params = payment::channel::Params::with(channel_req)?;
        self.remote_keys = payment::channel::Keyset::from(channel_req);

        let dumb_key = self.node_id();
        let accept_channel = message::AcceptChannel {
            temporary_channel_id: channel_req.temporary_channel_id,
            dust_limit_satoshis: channel_req.dust_limit_satoshis,
            max_htlc_value_in_flight_msat: channel_req
                .max_htlc_value_in_flight_msat,
            channel_reserve_satoshis: channel_req.channel_reserve_satoshis,
            htlc_minimum_msat: channel_req.htlc_minimum_msat,
            minimum_depth: 3, // TODO: take from config options
            to_self_delay: channel_req.to_self_delay,
            max_accepted_htlcs: channel_req.max_accepted_htlcs,
            funding_pubkey: dumb_key,
            revocation_basepoint: dumb_key,
            payment_point: dumb_key,
            delayed_payment_basepoint: dumb_key,
            htlc_basepoint: dumb_key,
            first_per_commitment_point: dumb_key,
            /* shutdown_scriptpubkey: None,
             * unknown_tlvs: none!(), */
        };

        self.params.updated(&accept_channel, None)?;
        self.local_keys = payment::channel::Keyset::from(&accept_channel);

        let msg = format!(
            "{} channel {:#} from remote peer {}",
            "Accepted".ended(),
            channel_req.temporary_channel_id.ender(),
            peerd.ender()
        );
        info!("{}", msg);
        let _ = self.report_success_to(senders, &enquirer, Some(msg));

        Ok(accept_channel)
    }

    pub fn channel_accepted(
        &mut self,
        senders: &mut Senders,
        accept_channel: &message::AcceptChannel,
        peerd: &ServiceId,
    ) -> Result<(), payment::channel::NegotiationError> {
        info!(
            "Channel {:#} {} by the remote peer {}",
            accept_channel.temporary_channel_id.ender(),
            "was accepted".ended(),
            peerd.ender()
        );
        // Ignoring possible reporting errors here and after: do not want to
        // halt the channel just because the client disconnected
        let enquirer = self.enquirer.clone();
        let _ = self.report_progress_to(
            senders,
            &enquirer,
            "Channel was accepted by the remote peer",
        );

        let msg = format!(
            "{} returned parameters for the channel {:#}",
            "Verifying".promo(),
            accept_channel.temporary_channel_id.promoter()
        );
        info!("{}", msg);

        // TODO: Add a reasonable min depth bound
        self.params.updated(accept_channel, None)?;
        self.remote_keys = payment::channel::Keyset::from(accept_channel);

        let msg = format!(
            "Channel {:#} is {}",
            accept_channel.temporary_channel_id.ender(),
            "ready for funding".ended()
        );
        info!("{}", msg);
        let _ = self.report_success_to(senders, &enquirer, Some(msg));

        Ok(())
    }

    pub fn fund_channel(
        &mut self,
        senders: &mut Senders,
        funding_outpoint: OutPoint,
    ) -> Result<message::FundingCreated, Error> {
        let enquirer = self.enquirer.clone();

        info!(
            "{} {}",
            "Funding channel".promo(),
            self.temporary_channel_id.promoter()
        );
        let _ = self.report_progress_to(
            senders,
            &enquirer,
            format!("Funding channel {:#}", self.temporary_channel_id),
        );

        self.funding_outpoint = funding_outpoint;
        self.funding_update(senders)?;

        let signature = self.sign_funding();
        let funding_created = message::FundingCreated {
            temporary_channel_id: self.temporary_channel_id,
            funding_txid: self.funding_outpoint.txid,
            funding_output_index: self.funding_outpoint.vout as u16,
            signature,
        };
        trace!("Prepared funding_created: {:?}", funding_created);

        let msg = format!(
            "{} for channel {:#}. Awaiting for remote node signature.",
            "Funding created".ended(),
            self.channel_id.ender()
        );
        info!("{}", msg);
        let _ = self.report_progress_to(senders, &enquirer, msg);

        Ok(funding_created)
    }

    pub fn funding_created(
        &mut self,
        senders: &mut Senders,
        funding_created: message::FundingCreated,
    ) -> Result<message::FundingSigned, Error> {
        let enquirer = self.enquirer.clone();

        info!(
            "{} {}",
            "Accepting channel funding".promo(),
            self.temporary_channel_id.promoter()
        );
        let _ = self.report_progress_to(
            senders,
            &enquirer,
            format!(
                "Accepting channel funding {:#}",
                self.temporary_channel_id
            ),
        );

        self.funding_outpoint = OutPoint {
            txid: funding_created.funding_txid,
            vout: funding_created.funding_output_index as u32,
        };
        // TODO: Save signature!
        self.funding_update(senders)?;

        let signature = self.sign_funding();
        let funding_signed = message::FundingSigned {
            channel_id: self.channel_id,
            signature,
        };
        trace!("Prepared funding_signed: {:?}", funding_signed);

        let msg = format!(
            "{} for channel {:#}. Awaiting for funding tx mining.",
            "Funding signed".ended(),
            self.channel_id.ender()
        );
        info!("{}", msg);
        let _ = self.report_progress_to(senders, &enquirer, msg);

        Ok(funding_signed)
    }

    pub fn funding_update(
        &mut self,
        senders: &mut Senders,
    ) -> Result<(), Error> {
        let mut engine = sha256::Hash::engine();
        if self.is_originator {
            engine.input(&self.local_keys.payment_basepoint.serialize());
            engine.input(&self.remote_keys.payment_basepoint.serialize());
        } else {
            engine.input(&self.remote_keys.payment_basepoint.serialize());
            engine.input(&self.local_keys.payment_basepoint.serialize());
        }
        let obscuring_hash = sha256::Hash::from_engine(engine);
        trace!("Obscuring hash: {}", obscuring_hash);

        let mut buf = [0u8; 8];
        buf.copy_from_slice(&obscuring_hash[24..]);
        self.obscuring_factor = u64::from_be_bytes(buf);
        trace!("Obscuring factor: {:#016x}", self.obscuring_factor);
        self.commitment_number = 0;

        self.update_channel_id(senders)?;

        Ok(())
    }

    pub fn sign_funding(&mut self) -> secp256k1::Signature {
        // We are doing counterparty's transaction!
        let mut cmt_tx = Transaction::ln_cmt_base(
            self.remote_capacity,
            self.local_capacity,
            self.commitment_number,
            self.obscuring_factor,
            self.funding_outpoint,
            self.local_keys.payment_basepoint,
            self.local_keys.revocation_basepoint,
            self.remote_keys.delayed_payment_basepoint,
            self.params.to_self_delay,
        );
        trace!("Counterparty's commitment tx: {:?}", cmt_tx);

        let mut sig_hasher = SigHashCache::new(&mut cmt_tx);
        let sighash = sig_hasher.signature_hash(
            0,
            &PubkeyScript::ln_funding(
                self.channel_capacity(),
                self.local_keys.funding_pubkey,
                self.remote_keys.funding_pubkey,
            )
            .into(),
            self.channel_capacity(),
            SigHashType::All,
        );
        let sign_msg = secp256k1::Message::from_slice(&sighash[..])
            .expect("Sighash size always match requirements");
        let signature = self.local_node.sign(&sign_msg);
        trace!("Commitment transaction signature created");
        // .serialize_der();
        // let mut with_hashtype = signature.to_vec();
        // with_hashtype.push(SigHashType::All.as_u32() as u8);

        signature
    }

    pub fn htlc_receive(
        &mut self,
        _senders: &mut Senders,
        update_add_htlc: message::UpdateAddHtlc,
    ) -> Result</* message::CommitmentSigned */ (), Error> {
        trace!("Updating HTLCs with {:?}", update_add_htlc);
        // TODO: Use From/To for message <-> Htlc conversion in LNP/BP
        //       Core lib
        let htlc = HtlcSecret {
            amount: update_add_htlc.amount_msat,
            hashlock: update_add_htlc.payment_hash,
            id: update_add_htlc.htlc_id,
            cltv_expiry: update_add_htlc.cltv_expiry,
            asset_id: update_add_htlc.asset_id,
        };
        self.received_htlc.push(htlc);

        let available = if let Some(asset_id) = update_add_htlc.asset_id {
            self.remote_balances.get(&asset_id).copied().unwrap_or(0)
        } else {
            self.remote_capacity
        };

        if available < update_add_htlc.amount_msat {
            Err(Error::Other(s!(
                "Remote node does not have required amount of the asset"
            )))?
        }

        self.total_payments += 1;
        match update_add_htlc.asset_id {
            Some(asset_id) => {
                self.remote_balances.get_mut(&asset_id).map(|balance| {
                    *balance -= update_add_htlc.amount_msat;
                });

                let entry = self.local_balances.entry(asset_id).or_insert(0);
                *entry += update_add_htlc.amount_msat;
            }
            None => {
                self.remote_capacity -= update_add_htlc.amount_msat;
                self.local_capacity += update_add_htlc.amount_msat;
            }
        }

        Ok(())

        // TODO:
        //      1. Generate new commitment tx
        //      2. Generate new transitions and anchor, commit into tx
        //      3. Sign commitment tx
        //      4. Generate HTLCs, tweak etc each of them
        //      3. Send response
    }
}
