#![doc = include_str!("../README.md")]

mod check;
mod findings;
mod mutate;
mod stats;

pub use check::Outcome;
pub use check::block_network;
pub use check::check;
pub use findings::Recheck;
pub use findings::recheck;
pub use findings::record;
pub use mutate::crossover;
pub use mutate::mutate;
pub use stats::count;
