//! Various utils to help build functional tests.

#![allow(dead_code)]

pub mod assert;
pub mod config;
pub mod fc;
pub mod misc;

pub fn setup_logging() {
    // !!! Configure RUST_LOG in CI to change this value !!!
    let env = env_logger::Env::new().default_filter_or("farcaster_node=debug");
    let _ = env_logger::from_env(env).is_test(true).try_init();
}
