pub mod app;
pub mod cli;
pub mod config;
pub mod error;
pub mod execute;
pub mod extract;
pub mod inference;
pub mod model;
pub mod paths;
pub mod platform;
pub mod runtime;
pub mod safety;
pub mod setup;
pub mod shell;

pub const NAME: &str = "HowTo";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
