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

use clap::Parser;
use internet2::addr::InetSocketAddr;
use nix::unistd::{fork, ForkResult};
use std::convert::TryFrom;
use std::net::TcpListener;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use farcaster_node::peerd::{self, Opts};
use farcaster_node::LogStyle;
use farcaster_node::ServiceConfig;
use internet2::addr::NodeAddr;
use internet2::session;
use microservices::peer::PeerConnection;

/// Chooses type of service runtime (see `--listen` and `--connect` option
/// details in [`Opts`] structure.
#[derive(Clone, PartialEq, Eq, Debug, Display)]
pub enum PeerSocket {
    /// The service should listen for incoming connections on a certain
    /// TCP socket, which may be IPv4- or IPv6-based. For Tor hidden services
    /// use IPv4 TCP port proxied as a Tor hidden service in `torrc`.
    #[display("--listen={0}")]
    Listen(InetSocketAddr),

    /// The service should connect to the remote peer residing on the provided
    /// address, which may be either IPv4/v6 or Onion V2/v3 address (using
    /// onion hidden services will require
    /// DNS names, due to a censorship vulnerability issues and for avoiding
    /// leaking any information about th elocal node to DNS resolvers, are not
    /// supported.
    #[display("--connect={0}")]
    Connect(NodeAddr),
}

impl From<Opts> for PeerSocket {
    fn from(opts: Opts) -> Self {
        if let Some(peer_addr) = opts.connect {
            Self::Connect(peer_addr)
        } else if let Some(bind_addr) = opts.listen {
            Self::Listen(InetSocketAddr::socket(
                bind_addr.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
                opts.port,
            ))
        } else {
            unreachable!("Either `connect` or `listen` must be present due to Clap configuration")
        }
    }
}

fn main() {
    let mut opts = Opts::parse();
    trace!("Command-line arguments: {:?}", &opts);
    opts.process();
    trace!("Processed arguments: {:?}", &opts);

    let service_config: ServiceConfig = opts.shared.clone().into();
    trace!("Daemon configuration: {:#?}", &service_config);
    debug!("MSG RPC socket {}", &service_config.msg_endpoint);
    debug!("CTL RPC socket {}", &service_config.ctl_endpoint);

    let local_node = opts.peer_key_opts.local_node();
    info!(
        "{}: {}",
        "Local node id".bright_green_bold(),
        local_node.node_id().bright_yellow_bold()
    );

    let peer_socket = PeerSocket::from(opts.clone());
    debug!("Peer socket parameter interpreted as {}", peer_socket);

    let mut local_socket: Option<InetSocketAddr> = None;
    let mut remote_node_addr: Option<NodeAddr> = None;
    let forked_from_listener: bool;
    let connection = match peer_socket {
        PeerSocket::Listen(inet_addr) => {
            debug!("Running in LISTEN mode");

            forked_from_listener = true;
            local_socket = Some(inet_addr);

            debug!("Binding TCP socket {}", inet_addr);
            if let Ok(listener) = TcpListener::bind(
                SocketAddr::try_from(inet_addr).expect("Tor is not yet supported"),
            ) {
                debug!("Running TCP listener event loop");
                loop {
                    debug!("Awaiting for incoming connections...");
                    let (stream, remote_socket_addr) = listener
                        .accept()
                        .expect("Error accepting incoming peer connection");
                    debug!("New connection from {}", remote_socket_addr);

                    // TODO: Support multithread mode
                    debug!("Forking child process");
                    if let ForkResult::Child =
                        unsafe { fork().expect("Unable to fork child process") }
                    {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(30)))
                            .expect("Unable to set up timeout for TCP connection");

                        debug!("Establishing session with the remote");
                        let session = session::BrontozaurSession::with(
                            stream,
                            local_node.private_key(),
                            inet_addr,
                        )
                        .expect("Unable to establish session with the remote peer");

                        debug!(
                            "Session successfully established with {}",
                            remote_socket_addr
                        );

                        break PeerConnection::with(session);
                    }
                    debug!("Child forked; returning into main listener event loop");
                    continue;
                }
            } else {
                error!("Unable to bind to {} socket", inet_addr.red_bold());
                std::process::exit(2);
            }
        }
        PeerSocket::Connect(remote_node) => {
            debug!("Peerd running in CONNECT mode");

            forked_from_listener = false;
            remote_node_addr = Some(remote_node);

            debug!("Connecting to {}", &remote_node.addr());

            let mut connection = PeerConnection::connect_brontozaur(local_node, remote_node);
            let mut attempt = 0;

            while let Err(err) = connection {
                trace!("failed to establish tcp connection: {}", err);
                attempt += 1;
                warn!("connect failed attempting again in {} seconds", attempt);
                std::thread::sleep(std::time::Duration::from_secs(attempt));
                connection = PeerConnection::connect_brontozaur(local_node, remote_node);
            }
            connection.expect("Already filtered for connection errors")
        }
    };

    debug!("Starting runtime ...");

    /* A maker / listener passes the following content
        remote_node_addr: none
        local_socket: local inet address
        connect: false

    A taker / connecter passes the following content
        remote_node_addr: full internet2 remote node address
        local_socket: None
        connect: true */
    peerd::run(
        service_config,
        connection,
        remote_node_addr,
        local_socket,
        local_node,
        forked_from_listener,
    )
    .expect("Error running peerd runtime");

    unreachable!()
}
