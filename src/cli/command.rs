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
use microservices::shell::Exec;

use farcaster_core::{
    blockchain::Network,
    negotiation::PublicOffer,
    role::{SwapRole, TradeRole},
    swap::SwapId,
};
use strict_encoding::ReadExt;

use super::Command;
use crate::{
    rpc::{request, Client, Request},
    syncerd::Coin,
};
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
                        runtime.request(ServiceId::Peer(node_addr), Request::GetInfo(None))?;
                    } else if let Ok(swap_id) = SwapId::from_str(&subj) {
                        runtime.request(ServiceId::Swap(swap_id), Request::GetInfo(None))?;
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
                    runtime.request(ServiceId::Farcasterd, Request::GetInfo(None))?;
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
                runtime.report_response_or_fail()?;
            }

            Command::ListSwaps => {
                runtime.request(ServiceId::Farcasterd, Request::ListSwaps)?;
                runtime.report_response_or_fail()?;
            }

            // TODO: only list offers matching list of OfferIds
            Command::ListOffers => {
                runtime.request(ServiceId::Farcasterd, Request::ListOffers)?;
                runtime.report_response_or_fail()?;
            }

            Command::ListListens => {
                runtime.request(ServiceId::Farcasterd, Request::ListListens)?;
                runtime.report_response_or_fail()?;
            }

            Command::ListCheckpoints => {
                runtime.request(ServiceId::Checkpoint, Request::RetrieveAllCheckpointInfo)?;
                runtime.report_response_or_fail()?;
            }

            Command::RestoreCheckpoint { swap_id } => {
                runtime.request(ServiceId::Farcasterd, Request::RestoreCheckpoint(swap_id))?;
                runtime.report_response_or_fail()?;
            }

            // Command::ListOfferIds => {
            //     runtime.request(ServiceId::Farcasterd, Request::ListOfferIds)?;
            //     runtime.report_response_or_fail()?;
            // }
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
                if network != Network::Testnet && network != Network::Local {
                    eprintln!(
                        "Error: {} not yet supported. Only Testnet and Local currently enabled, for your funds safety",
                        network
                    );
                    return Ok(());
                }
                if accordant_amount < monero::Amount::from_str("0.001 XMR").unwrap() {
                    eprintln!(
                        "Error: monero amount {} too low, require at least 0.001 XMR",
                        accordant_amount
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
                    peer_public_key: None,
                    arbitrating_addr,
                    accordant_addr: accordant_addr.to_string(),
                };
                runtime.request(ServiceId::Farcasterd, Request::MakeOffer(proto_offer))?;
                // report success or failure of the request to cli
                runtime.report_response_or_fail()?;
            }

            Command::Take {
                public_offer,
                bitcoin_address,
                monero_address,
                without_validation,
            } => {
                let network = public_offer.offer.network;
                if network != Network::Testnet && network != Network::Local {
                    eprintln!(
                        "Error: {} not yet supported. Only Testnet and Local currently enabled, for your funds safety",
                        network
                    );
                    return Ok(());
                }
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
                            SwapRole::Alice => format!(
                                "{} for {} at {} BTC/XMR",
                                arb_amount,
                                acc_amount,
                                arb_amount.as_btc() / acc_amount.as_xmr()
                            ),
                            SwapRole::Bob => format!(
                                "{} for {} at {} XMR/BTC",
                                acc_amount,
                                arb_amount,
                                acc_amount.as_xmr() / arb_amount.as_btc()
                            ),
                        }
                    );
                    println!("Trade counterparty: {}@{}\n", &node_id, peer_address);
                    println!("{}\n", offer);
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
                    runtime.report_response_or_fail()?;
                }
            }

            Command::RevokeOffer { public_offer } => {
                runtime.request(ServiceId::Farcasterd, Request::RevokeOffer(public_offer))?;
                runtime.report_response_or_fail()?;
            }

            Command::Progress { swapid, follow } => {
                if follow {
                    // subscribe to progress event and loop until Finish event is received or user
                    // ctrl-c the cli. Expect to recieve a stream of event responses
                    runtime.request(ServiceId::Farcasterd, Request::SubscribeProgress(swapid))?;
                    let res = runtime.report_progress();
                    // if user didn't ctrl-c before that point we can cleanly unsubscribe the
                    // client from the notification stream and then return the result from report
                    // progress
                    runtime.request(ServiceId::Farcasterd, Request::UnsubscribeProgress(swapid))?;
                    return res;
                } else {
                    // request a read progress response. Expect to recieve only one response and
                    // quit
                    runtime.request(ServiceId::Farcasterd, Request::ReadProgress(swapid))?;
                    runtime.report_response_or_fail()?;
                }
            }

            Command::NeedsFunding { coin } => {
                runtime.request(ServiceId::Farcasterd, Request::NeedsFunding(coin))?;
                runtime.report_response_or_fail()?;
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
