//! Various utils to help build functional tests.

// dead code is allowed because there is a possible error in Cargo that warns
// about dead functions even if they are used;
// see https://github.com/rust-lang/rust/issues/46379
// a possible workaround is https://github.com/rust-lang/rust/issues/46379#issuecomment-487421236
// but as described in https://github.com/rust-lang/rust/issues/46379#issuecomment-487544472,
// such solution hides code that is truly dead.
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
    // try init the logger; this fails if logger has been already initialized
    // and it happens when multiple tests are called as the (test)binary is not restarted
    let _ = env_logger::from_env(env).is_test(false).try_init();
}
