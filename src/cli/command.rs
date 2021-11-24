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

use std::{
    convert::TryFrom,
    io::{self, Read, Write},
    sync::mpsc,
    time::Duration,
};
use std::{str::FromStr, thread::sleep};

use internet2::{NodeAddr, RemoteSocketAddr, ToNodeAddr};
use lnp::{message, LIGHTNING_P2P_DEFAULT_PORT};
use microservices::shell::Exec;

use farcaster_core::{
    blockchain::Network,
    negotiation::PublicOffer,
    role::{SwapRole, TradeRole},
    swap::SwapId,
};
use strict_encoding::ReadExt;

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
                        runtime.request(ServiceId::Peer(node_addr), Request::GetInfo)?;
                    } else if let Ok(swap_id) = SwapId::from_str(&subj) {
                        runtime.request(ServiceId::Swap(swap_id), Request::GetInfo)?;
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
                    _ => {
                        return Err(Error::Other(
                            "Server returned unrecognizable response".to_string(),
                        ))
                    }
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
                let socket = RemoteSocketAddr::with_ip_addr(overlay, ip_addr, port);
                runtime.request(ServiceId::Farcasterd, Request::Listen(socket))?;
                runtime.report_progress()?;
            }

            Command::Connect { peer: node_locator } => {
                let peer = node_locator
                    .to_node_addr(LIGHTNING_P2P_DEFAULT_PORT)
                    .ok_or(internet2::presentation::Error::InvalidEndpoint)?;

                runtime.request(ServiceId::Farcasterd, Request::ConnectPeer(peer))?;
                runtime.report_progress()?;
            }

            Command::Ping { peer } => {
                let node_addr = peer
                    .to_node_addr(LIGHTNING_P2P_DEFAULT_PORT)
                    .ok_or(internet2::presentation::Error::InvalidEndpoint)?;

                runtime.request(ServiceId::Peer(node_addr), Request::PingPeer)?;
            }
            Command::Make {
                network,
                arbitrating_blockchain,
                accordant_blockchain,
                arbitrating_amount,
                accordant_amount,
                arbitrating_addr,
                accordant_addr,
                cancel_timelock,
                punish_timelock,
                fee_strategy,
                maker_role,
                public_ip_addr,
                bind_ip_addr,
                port,
                overlay,
            } => {
                if network != Network::Testnet {
                    eprintln!(
                        "Error: {} not yet supported. Only Testnet currently enabled, for your funds safety",
                        network
                    );
                    return Ok(());
                }
                let offer = farcaster_core::negotiation::Offer {
                    network,
                    arbitrating_blockchain,
                    accordant_blockchain,
                    arbitrating_amount,
                    accordant_amount,
                    cancel_timelock,
                    punish_timelock,
                    fee_strategy,
                    maker_role,
                };
                let public_addr = RemoteSocketAddr::with_ip_addr(overlay, public_ip_addr, port);
                let bind_addr = RemoteSocketAddr::with_ip_addr(overlay, bind_ip_addr, port);
                let proto_offer = request::ProtoPublicOffer {
                    offer,
                    public_addr,
                    bind_addr,
                    peer_secret_key: None,
                    arbitrating_addr,
                    accordant_addr: accordant_addr.to_string(),
                };
                runtime.request(ServiceId::Farcasterd, Request::MakeOffer(proto_offer))?;
                // report success of failure of the request to cli
                runtime.report_progress()?;
                // TODO: activate when we do client side offer validation, must
                // be activated on farcasterd as well
                // let public_offer = runtime.response()?;
                // let instruction =
                //     format!("Share the following offer with taker:",);
                // let hex = format!("{:?}", &public_offer);
                // println!("{} \n {}", instruction.green_bold(),
                // hex.bright_yellow_bold());
            }

            Command::Take {
                public_offer,
                bitcoin_address,
                monero_address,
                without_validation,
            } => {
                // println!("{:#?}", &public_offer);
                let PublicOffer {
                    version: _,
                    offer,
                    node_id,
                    peer_address,
                } = public_offer.clone();
                if !without_validation {
                    let taker_role = offer.maker_role.other();
                    let arb_amount = offer.arbitrating_amount;
                    let acc_amount = offer.accordant_amount;
                    println!(
                        "\nWant to buy {}?\n\nCarefully validate offer!\n",
                        match taker_role {
                            SwapRole::Alice => format!("{} for {}", arb_amount, acc_amount),
                            SwapRole::Bob => format!("{} for {}", acc_amount, arb_amount),
                        }
                    );
                    println!("Trade counterparty: {}@{}\n", &node_id, peer_address);
                    println!("{:#?}\n", offer);
                }
                if without_validation || take_offer() {
                    // pass offer to farcasterd to initiate the swap
                    runtime.request(
                        ServiceId::Farcasterd,
                        Request::TakeOffer(
                            (public_offer, bitcoin_address, monero_address.to_string()).into(),
                        ),
                    )?;
                    // report success of failure of the request to cli
                    runtime.report_progress()?;
                }
            }
            Command::Progress { swapid } => {
                runtime.request(ServiceId::Farcasterd, Request::ReadProgress(swapid))?;
                runtime.report_progress()?;
            }
        }
        Ok(())
    }
}

fn take_offer() -> bool {
    println!("Take it? [y/n]");
    let mut input = [0u8; 1];
    std::io::stdin().read_exact(&mut input).unwrap_or(());
    match std::str::from_utf8(&input[..]) {
        Ok("y") | Ok("Y") => true,
        Ok("n") | Ok("N") => {
            println!("Rejecting offer");
            false
        }
        _ => take_offer(),
    }
}
