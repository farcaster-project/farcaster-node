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

use std::convert::TryFrom;
use std::str::FromStr;

use internet2::{
    NodeAddr, RemoteNodeAddr, RemoteSocketAddr, ToNodeAddr, ToRemoteNodeAddr,
};
use lnp::{message, ChannelId as SwapId, LIGHTNING_P2P_DEFAULT_PORT};
use microservices::shell::Exec;

use super::Command;
use crate::rpc::{request, Client, Request};
use crate::{Error, LogStyle, ServiceId};

impl Exec for Command {
    type Client = Client;
    type Error = Error;

    fn exec(self, runtime: &mut Self::Client) -> Result<(), Self::Error> {
        debug!("Performing {:?}: {}", self, self);
        match self {
            Command::Info { subject } => {
                if let Some(subj) = subject {
                    if let Ok(node_addr) = NodeAddr::from_str(&subj) {
                        runtime.request(
                            ServiceId::Peer(node_addr),
                            Request::GetInfo,
                        )?;
                    } else if let Ok(channel_id) = SwapId::from_str(&subj) {
                        runtime.request(
                            ServiceId::Swaps(channel_id),
                            Request::GetInfo,
                        )?;
                    } else {
                        let err = format!(
                            "{}",
                            "Subject parameter must be either remote node \
                            address or channel id represented by a hex string"
                                .err()
                        );
                        return Err(Error::Other(err));
                    }
                } else {
                    // subject is none
                    runtime.request(ServiceId::Farcasterd, Request::GetInfo)?;
                }
                match runtime.response()? {
                    Request::NodeInfo(info) => println!("{}", info),
                    Request::PeerInfo(info) => println!("{}", info),
                    Request::SwapInfo(info) => println!("{}", info),
                    _ => Err(Error::Other(format!(
                        "{}",
                        "Server returned unrecognizable response"
                    )))?,
                }
            }

            Command::Peers => {
                runtime.request(ServiceId::Farcasterd, Request::ListPeers)?;
                runtime.report_response()?;
            }

            Command::Ls => {
                runtime.request(ServiceId::Farcasterd, Request::ListSwaps)?;
                runtime.report_response()?;
            }

            Command::Listen {
                ip_addr,
                port,
                overlay,
            } => {
                let socket =
                    RemoteSocketAddr::with_ip_addr(overlay, ip_addr, port);
                runtime
                    .request(ServiceId::Farcasterd, Request::Listen(socket))?;
                runtime.report_progress()?;
            }

            Command::Connect { peer: node_locator } => {
                let peer = node_locator
                    .to_node_addr(LIGHTNING_P2P_DEFAULT_PORT)
                    .ok_or_else(|| internet2::presentation::Error::InvalidEndpoint)?;

                runtime.request(
                    ServiceId::Farcasterd,
                    Request::ConnectPeer(peer),
                )?;
                runtime.report_progress()?;
            }

            Command::Ping { peer } => {
                let node_addr = peer
                    .to_node_addr(LIGHTNING_P2P_DEFAULT_PORT)
                    .ok_or_else(|| internet2::presentation::Error::InvalidEndpoint)?;

                runtime
                    .request(ServiceId::Peer(node_addr), Request::PingPeer)?;
            }
            Command::MakeOffer {
                network,
                arbitrating,
                accordant,
                arbitrating_assets,
                accordant_assets,
                cancel_timelock,
                punish_timelock,
                fee_strategy,
                maker_role,
            } => {
                use farcaster_core::negotiation::{Offer, PublicOffer};
                let offer = Offer {
                    network,
                    arbitrating,
                    accordant,
                    arbitrating_assets,
                    accordant_assets,
                    cancel_timelock,
                    punish_timelock,
                    fee_strategy,
                    maker_role,
                };

                runtime.request(ServiceId::Farcasterd, Request::GetInfo)?;
                let node_id = match runtime.response()? {
                    Request::NodeInfo(info) => Ok(info.node_id),
                    _ => Err(Error::Rpc(
                        microservices::rpc::Error::UnexpectedServerResponse,
                    )),
                }?;

                // FIXME following should not be hardcoded
                use std::str::FromStr;
                let overlay = FromStr::from_str("tcp")
                    .map_err(|_| Error::Other("Parsing overlay".to_string()))?;
                let ip = FromStr::from_str("0.0.0.0")
                    .map_err(|_| Error::Other("Parsing ip".to_string()))?;
                let port = FromStr::from_str("9735")
                    .map_err(|_| Error::Other("Parsing port".to_string()))?;
                let remote_addr =
                    RemoteSocketAddr::with_ip_addr(overlay, ip, port);

                // Start listening
                runtime.request(
                    ServiceId::Farcasterd,
                    Request::Listen(remote_addr),
                )?;
                // Report to cli
                runtime.report_progress()?;
                // Create public offer
                let peer = RemoteNodeAddr {
                    node_id,
                    remote_addr,
                };
                let public_offer = offer.to_public_v1(peer);
                println!("{}", "\nPlease share the following Offer with Taker: \n");
                println!("{:?}", &public_offer.to_string());
            }

            Command::TakeOffer { public_offer } => {
                let peer = public_offer
                    .daemon_service
                    .to_node_addr(LIGHTNING_P2P_DEFAULT_PORT)
                    .ok_or_else(|| internet2::presentation::Error::InvalidEndpoint)?;
                // connect to counterparty node
                runtime.request(
                    ServiceId::Farcasterd,
                    Request::ConnectPeer(peer),
                )?;
                //report progress to cli
                runtime.report_progress()?;
            }
            _ => unimplemented!(),
        }
        Ok(())
    }
}
