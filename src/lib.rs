//! Bruce's Method: Release. Developing and releasing a set of Rust repositories which depend on one another, cloned
//! side by side: libraries published to crates.io, and the apps which use them, see the README.
//!
//! The commands are in [commands], each taking the repository to work in and a [ui::Ui] to report to and confirm
//! with, so that a desktop window can drive them as the command line does.

pub mod commands;
pub mod links;
pub mod manifest;
pub mod process;
pub mod repo;
pub mod ui;
pub mod version;

pub use repo::Repo;
pub use version::Version;
