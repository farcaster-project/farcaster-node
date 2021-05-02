use clap::Clap;
use farcaster_node::rpc::Client;
use farcaster_node::{Config, LogStyle};

use farcaster_node::cli::Command;
use microservices::shell::Exec;

#[test]
fn spawn_swap() {
    use farcaster_node::farcasterd::{launch, Opts};
    let mut opts = Opts::parse_from(vec![""].into_iter());
    opts.process();
    println!("{:?}", opts);
    let config: Config = opts.shared.clone().into();

    let mut client = Client::with(config.clone(), config.chain.clone())
        .expect("Error running client");

    launch("../farcasterd", std::vec::Vec::<&str>::new(), false).unwrap();

    Command::Ls
        .exec(&mut client)
        .unwrap_or_else(|err| eprintln!("{} {}", "error:".err(), err.err()));
}
