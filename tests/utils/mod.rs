//! Various utils to help build functional tests.

#![allow(dead_code)]

pub mod assert;
pub mod config;
pub mod fc;
pub mod misc;

pub fn setup_logging(level: Option<log::LevelFilter>) {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(level.unwrap_or(log::LevelFilter::Info))
        .try_init();
}
