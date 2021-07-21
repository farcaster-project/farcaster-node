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

use std::convert::TryInto;
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use bitcoin::hashes::hex::{self, ToHex};
use internet2::{zmqsocket, NodeAddr, ZmqType};
// use lnp::{TempChannelId as TempSwapId};
use lnpbp::strict_encoding::{strict_deserialize, strict_serialize};
#[cfg(feature = "node")]
use microservices::node::TryService;
use microservices::{esb, rpc};

use farcaster_core::swap::SwapId;

use crate::rpc::{Request, ServiceBus};
use crate::Config;
use crate::Error;

#[derive(
    Wrapper,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Debug,
    From,
    Default,
    StrictEncode,
    StrictDecode,
)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
pub struct ClientName([u8; 32]);

impl Display for ClientName {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            write!(
                f,
                "{}..{}",
                self.0[..4].to_hex(),
                self.0[(self.0.len() - 4)..].to_hex()
            )
        } else {
            f.write_str(&String::from_utf8_lossy(&self.0))
        }
    }
}

impl FromStr for ClientName {
    type Err = hex::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() > 32 {
            let mut me = Self::default();
            me.0.copy_from_slice(&s.as_bytes()[0..32]);
            Ok(me)
        } else {
            let mut me = Self::default();
            me.0[0..s.len()].copy_from_slice(s.as_bytes());
            Ok(me)
        }
    }
}

/// Identifiers of daemons participating in LNP Node
#[derive(Clone, PartialEq, Eq, Hash, Debug, Display, From, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
pub enum ServiceId {
    #[display("loopback")]
    Loopback,

    #[display("farcasterd")]
    Farcasterd,

    #[display("peerd<{0}>")]
    #[from]
    Peer(NodeAddr),

    #[display("swap<{0:#x}>")]
    #[from]
    // #[from(TempSwapId)]
    Swap(SwapId),

    #[display("client<{0}>")]
    Client(u64),

    #[display("syncer")]
    Syncer,

    #[display("walletd")]
    Wallet,

    #[display("other<{0}>")]
    Other(ClientName),
}

impl ServiceId {
    pub fn router() -> ServiceId {
        ServiceId::Farcasterd
    }

    pub fn client() -> ServiceId {
        use bitcoin::secp256k1::rand;
        ServiceId::Client(rand::random())
    }
}

impl esb::ServiceAddress for ServiceId {}

impl From<ServiceId> for Vec<u8> {
    fn from(daemon_id: ServiceId) -> Self {
        strict_serialize(&daemon_id).expect("Memory-based encoding does not fail")
    }
}

impl From<Vec<u8>> for ServiceId {
    fn from(vec: Vec<u8>) -> Self {
        strict_deserialize(&vec).unwrap_or_else(|_| {
            ServiceId::Other(
                ClientName::from_str(&String::from_utf8_lossy(&vec))
                    .expect("ClientName conversion never fails"),
            )
        })
    }
}

pub struct Service<Runtime>
where
    Runtime: esb::Handler<ServiceBus, Address = ServiceId, Request = Request>,
    esb::Error: From<Runtime::Error>,
{
    esb: esb::Controller<ServiceBus, Request, Runtime>,
    broker: bool,
}

impl<Runtime> Service<Runtime>
where
    Runtime: esb::Handler<ServiceBus, Address = ServiceId, Request = Request>,
    esb::Error: From<Runtime::Error>,
{
    #[cfg(feature = "node")]
    pub fn run(config: Config, runtime: Runtime, broker: bool) -> Result<(), Error> {
        let service = Self::with(config, runtime, broker)?;
        service.run_loop()?;
        unreachable!()
    }

    fn with(config: Config, runtime: Runtime, broker: bool) -> Result<Self, esb::Error> {
        let router = if !broker {
            Some(ServiceId::router())
        } else {
            None
        };
        let esb = esb::Controller::with(
            map! {
                ServiceBus::Msg => esb::BusConfig::with_locator(
                        config.msg_endpoint.try_into()
                            .expect("Only ZMQ RPC is currently supported"),
                    router.clone()
                ),
                ServiceBus::Ctl => esb::BusConfig::with_locator(
                    config.ctl_endpoint.try_into()
                        .expect("Only ZMQ RPC is currently supported"),
                    router
                )
            },
            runtime,
            if broker {
                ZmqType::RouterBind
            } else {
                ZmqType::RouterConnect
            },
        )?;
        Ok(Self { esb, broker })
    }

    pub fn broker(config: Config, runtime: Runtime) -> Result<Self, esb::Error> {
        Self::with(config, runtime, true)
    }

    pub fn service(config: Config, runtime: Runtime) -> Result<Self, esb::Error> {
        Self::with(config, runtime, false)
    }

    pub fn is_broker(&self) -> bool {
        self.broker
    }

    pub fn add_loopback(&mut self, socket: zmq::Socket) -> Result<(), esb::Error> {
        self.esb.add_service_bus(
            ServiceBus::Bridge,
            esb::BusConfig {
                carrier: zmqsocket::Carrier::Socket(socket),
                router: None,
                queued: true,
            },
        )
    }

    #[cfg(feature = "node")]
    pub fn run_loop(mut self) -> Result<(), Error> {
        if !self.is_broker() {
            std::thread::sleep(core::time::Duration::from_secs(1));
            self.esb
                .send_to(ServiceBus::Ctl, ServiceId::Farcasterd, Request::Hello)?;
            self.esb
                .send_to(ServiceBus::Msg, ServiceId::Farcasterd, Request::Hello)?;
        }

        let identity = self.esb.handler().identity();
        info!("{} started", identity);

        self.esb.run_or_panic(&identity.to_string());

        unreachable!()
    }
}

pub type Senders = esb::SenderList<ServiceBus, ServiceId>;

pub trait TryToServiceId {
    fn try_to_service_id(&self) -> Option<ServiceId>;
}

impl TryToServiceId for ServiceId {
    fn try_to_service_id(&self) -> Option<ServiceId> {
        Some(self.clone())
    }
}

impl TryToServiceId for &Option<ServiceId> {
    fn try_to_service_id(&self) -> Option<ServiceId> {
        (*self).clone()
    }
}

impl TryToServiceId for Option<ServiceId> {
    fn try_to_service_id(&self) -> Option<ServiceId> {
        self.clone()
    }
}

pub trait CtlServer
where
    Self: esb::Handler<ServiceBus, Address = ServiceId>,
    esb::Error: From<Self::Error>,
{
    fn report_success_to(
        &mut self,
        senders: &mut Senders,
        dest: impl TryToServiceId,
        msg: Option<impl ToString>,
    ) -> Result<(), Error> {
        if let Some(dest) = dest.try_to_service_id() {
            senders.send_to(
                ServiceBus::Ctl,
                self.identity(),
                dest,
                Request::Success(msg.map(|m| m.to_string()).into()),
            )?;
        }
        Ok(())
    }

    fn report_progress_to(
        &mut self,
        senders: &mut Senders,
        dest: impl TryToServiceId,
        msg: impl ToString,
    ) -> Result<(), Error> {
        if let Some(dest) = dest.try_to_service_id() {
            senders.send_to(
                ServiceBus::Ctl,
                self.identity(),
                dest,
                Request::Progress(msg.to_string()),
            )?;
        }
        Ok(())
    }

    fn report_failure_to(
        &mut self,
        senders: &mut Senders,
        dest: impl TryToServiceId,
        failure: impl Into<rpc::Failure>,
    ) -> Error {
        let failure = failure.into();
        if let Some(dest) = dest.try_to_service_id() {
            // Even if we fail, we still have to terminate :)
            let _ = senders.send_to(
                ServiceBus::Ctl,
                self.identity(),
                dest,
                Request::Failure(failure.clone()),
            );
        }
        Error::Terminate(failure.to_string())
    }

    fn send_ctl(
        &mut self,
        senders: &mut Senders,
        dest: impl TryToServiceId,
        request: Request,
    ) -> Result<(), Error> {
        if let Some(dest) = dest.try_to_service_id() {
            senders.send_to(ServiceBus::Ctl, self.identity(), dest, request)?;
        }
        Ok(())
    }

    fn send_wallet(&mut self, bus: ServiceBus, senders: &mut Senders, request: Request) -> Result<(), Error> {
        let source = self.identity();
        trace!("sending {} to walletd from {}", request, source);
        senders
            .send_to(bus, source, ServiceId::Wallet, request)
            .map_err(From::from)
    }
}

// TODO: Move to LNP/BP Services library
use colored::Colorize;

pub trait LogStyle: ToString {
    fn promo(&self) -> colored::ColoredString {
        self.to_string().bold().bright_blue()
    }

    fn promoter(&self) -> colored::ColoredString {
        self.to_string().italic().bright_blue()
    }

    fn progress(&self) -> colored::ColoredString {
        self.to_string().bold().green()
    }

    fn ended(&self) -> colored::ColoredString {
        self.to_string().bold().bright_green()
    }

    fn ender(&self) -> colored::ColoredString {
        self.to_string().italic().bright_green()
    }

    fn amount(&self) -> colored::ColoredString {
        self.to_string().bold().bright_yellow()
    }

    fn addr(&self) -> colored::ColoredString {
        self.to_string().bold().bright_yellow()
    }

    fn err(&self) -> colored::ColoredString {
        self.to_string().bold().bright_red()
    }

    fn err_details(&self) -> colored::ColoredString {
        self.to_string().bold().red()
    }
}

impl<T> LogStyle for T where T: ToString {}
