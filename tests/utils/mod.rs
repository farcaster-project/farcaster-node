//! Various utils to help build functional tests.

#![allow(dead_code)]

pub mod assert;
pub mod config;
pub mod fc;
pub mod misc;

pub fn setup_logging() {
    // !!! Configure RUST_LOG in CI to change this value !!!
    // set default level to info, debug for node
    // info level is for bitcoin and monero that correspond to `tests/{bitcoin.rs,monero.rs}`
    let env = env_logger::Env::new().default_filter_or("info,farcaster_node=debug");
    // try init the logger, this fails if logger has been already initialized
    // and it happens when multiple tests are called as the (test)binary is not restarted
    let _ = env_logger::from_env(env).try_init();
}
