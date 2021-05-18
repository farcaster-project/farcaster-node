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

#![recursion_limit = "256"]
// Coding conventions
#![deny(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    unused_mut,
    unused_imports,
    dead_code,
    missing_docs
)]

//! Main executable for peerd: lightning peer network connection
//! microservice.
//!
//! Program operations
//! ==================
//!
//! Bootstrapping
//! -------------
//!
//! Since this daemon must operate external P2P TCP socket, and TCP socket can
//! be either connected to the remote or accept remote connections; and we need
//! a daemon per connection, while the incoming TCP socket can't be transferred
//! between processes using IPC, the only option here is to have two special
//! cases of the daemon.
//!
//! The first one will open TCP socket in listening mode and wait for incoming
//! connections, forking on each one of them, passing the accepted TCP socket to
//! the child and continuing on listening to the new connections. (In
//! multi-thread mode, differentiated with `--threaded` argument, instead of
//! forking damon will launch a new thread).
//!
//! The second one will be launched by some control process and then commanded
//! (through command API) to connect to a specific remote TCP socket.
//!
//! These two cases are differentiated by a presence command-line option
//! `--listen` followed by a listening address to bind (IPv4, IPv6, Tor and TCP
//! port number) or `--connect` option followed by a remote address in the same
//! format.
//!
//! Runtime
//! -------
//!
//! The overall program logic thus is the following:
//!
//! In the process starting from `main()`:
//! - Parse cli arguments into a config. There is no config file, since the
//!   daemon can be started only from another control process (`lnpd`) or by
//!   forking itself.
//! - If `--listen` argument is present, start a listening version as described
//!   above and open TCP port in listening mode; wait for incoming connections
//! - If `--connect` argument is present, connect to the remote TCP peer
//!
//! In forked/spawned version:
//! - Acquire connected TCP socket from the parent
//!
//! From here, all actions must be taken by either forked version or by a daemon
//! launched from the control process:
//! - Split TCP socket and related transcoders into reading and writing parts
//! - Create bridge ZMQ PAIR socket
//! - Put both TCP socket reading ZMQ bridge write PAIR parts into a thread
//!   ("bridge")
//! - Open control interface socket
//! - Create run loop in the main thread for polling three ZMQ sockets:
//!   * control interface
//!   * LN P2P messages from intranet
//!   * bridge socket
//!
//! Node key
//! --------
//!
//! Node key, used for node identification and in generation of the encryption
//! keys, is read from the file specified in `--key-file` parameter, or (if the
//! parameter is absent) from `LNP_NODE_KEY_FILE` environment variable.

#[macro_use]
extern crate log;
#[macro_use]
extern crate amplify_derive;

use clap::Clap;
use internet2::addr::InetSocketAddr;
use nix::unistd::{fork, ForkResult};
use std::convert::TryFrom;
use std::net::TcpListener;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use bitcoin::secp256k1::PublicKey;
use farcaster_node::peerd::{self, Opts};
use farcaster_node::{Config, LogStyle};
use internet2::{session, FramingProtocol, NodeAddr, RemoteNodeAddr, RemoteSocketAddr};
use microservices::peer::PeerConnection;

/*
mod internal {
    #![allow(unused)]
    include!(concat!(env!("OUT_DIR"), "/configure_me_config.rs"));
}
 */

/// Chooses type of service runtime (see `--listen` and `--connect` option
/// details in [`Opts`] structure.
#[derive(Clone, PartialEq, Eq, Debug, Display)]
pub enum PeerSocket {
    /// The service should listen for incoming connections on a certain
    /// TCP socket, which may be IPv4- or IPv6-based. For Tor hidden services
    /// use IPv4 TCP port proxied as a Tor hidden service in `torrc`.
    #[display("--listen={0}")]
    Listen(RemoteSocketAddr),

    /// The service should connect to the remote peer residing on the provided
    /// address, which may be either IPv4/v6 or Onion V2/v3 address (using
    /// onion hidden services will require
    /// DNS names, due to a censorship vulnerability issues and for avoiding
    /// leaking any information about th elocal node to DNS resolvers, are not
    /// supported.
    #[display("--connect={0}")]
    Connect(RemoteNodeAddr),
}

impl From<Opts> for PeerSocket {
    fn from(opts: Opts) -> Self {
        if let Some(peer_addr) = opts.connect {
            Self::Connect(peer_addr)
        } else if let Some(bind_addr) = opts.listen {
            Self::Listen(match opts.overlay {
                FramingProtocol::FramedRaw => RemoteSocketAddr::Ftcp(InetSocketAddr {
                    address: bind_addr
                        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
                        .into(),
                    port: opts.port,
                }),
                // TODO: (v2) implement overlay protocols
                _ => unimplemented!(),
            })
        } else {
            unreachable!("Either `connect` or `listen` must be present due to Clap configuration")
        }
    }
}

fn main() {
    println!("peerd: lightning peer network connection microservice");

    let mut opts = Opts::parse();
    trace!("Command-line arguments: {:?}", &opts);
    opts.process();
    trace!("Processed arguments: {:?}", &opts);

    let config: Config = opts.shared.clone().into();
    trace!("Daemon configuration: {:?}", &config);
    debug!("MSG RPC socket {}", &config.msg_endpoint);
    debug!("CTL RPC socket {}", &config.ctl_endpoint);


    /*
    use self::internal::ResultExt;
    let (config_from_file, _) =
        internal::Config::custom_args_and_optional_files(std::iter::empty::<
            &str,
        >())
        .unwrap_or_exit();
     */

    let local_node = opts.peer_key_opts.local_node();
    let local_id = local_node.node_id();
    info!("{}: {}", "Local node id".ended(), local_id.amount());
    let peer_socket = PeerSocket::from(opts);
    debug!("Peer socket parameter interpreted as {}", peer_socket);

    let id: NodeAddr;
    let mut local_socket: Option<InetSocketAddr> = None;
    let mut remote_id: Option<PublicKey> = None;
    let mut remote_socket: InetSocketAddr;
    let connect: bool;
    let connection = match peer_socket {
        PeerSocket::Listen(RemoteSocketAddr::Ftcp(inet_addr)) => {
            debug!("Running in LISTEN mode");

            connect = false;
            local_socket = Some(inet_addr);
            id = NodeAddr::Remote(RemoteNodeAddr {
                node_id: local_id,
                remote_addr: RemoteSocketAddr::Ftcp(inet_addr),
            });

            debug!("Binding TCP socket {}", inet_addr);
            let listener = TcpListener::bind(
                SocketAddr::try_from(inet_addr).expect("Tor is not yet supported"),
            )
            .expect("Unable to bind to Lightning network peer socket");

            debug!("Running TCP listener event loop");
            loop {
                debug!("Awaiting for incoming connections...");
                let (stream, remote_socket_addr) = listener
                    .accept()
                    .expect("Error accepting incoming peer connection");
                debug!("New connection from {}", remote_socket_addr);

                remote_socket = remote_socket_addr.into();

                // TODO: Support multithread mode
                debug!("Forking child process");
                if let ForkResult::Child = unsafe { fork().expect("Unable to fork child process") }
                {
                    trace!("Child forked; returning into main listener event loop");
                    continue;
                }

                stream
                    .set_read_timeout(Some(Duration::from_secs(30)))
                    .expect("Unable to set up timeout for TCP connection");

                debug!("Establishing session with the remote");
                let session = session::Raw::with_ftcp_unencrypted(stream, inet_addr)
                    .expect("Unable to establish session with the remote peer");

                debug!("Session successfully established");
                break PeerConnection::with(session);
            }
        }
        PeerSocket::Connect(remote_node_addr) => {
            debug!("Running in CONNECT mode");

            connect = true;
            id = NodeAddr::Remote(remote_node_addr.clone());
            remote_id = Some(remote_node_addr.node_id);
            remote_socket = remote_node_addr.remote_addr.into();

            info!("Connecting to {}", &remote_node_addr);
            PeerConnection::connect(remote_node_addr, &local_node)
                .expect("Unable to connect to the remote peer")
        }
        _ => unimplemented!(),
    };

    debug!("Starting runtime ...");
    peerd::run(
        config,
        connection,
        id,
        local_id,
        remote_id,
        local_socket,
        remote_socket,
        connect,
    )
    .expect("Error running peerd runtime");

    unreachable!()
}
