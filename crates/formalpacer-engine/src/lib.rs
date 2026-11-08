//! Everything FormalPacer does apart from drawing: finding games, reading displays, choosing
//! caps, and on Windows measuring games from outside and handing their caps to the driver. The app and `formalpacer --headless` both drive it through [`engine::start`].

pub mod config;
pub mod display;
#[cfg(windows)]
mod driver;
#[cfg(windows)]
mod elevate;
pub mod engine;
#[cfg(windows)]
mod floating;
pub mod layer;
#[cfg(windows)]
mod nvidia;
pub mod pacing;
pub mod placement;
#[cfg(windows)]
mod presentmon;
mod process;
#[cfg(windows)]
mod registry;
#[cfg(windows)]
mod rtss;
pub mod steam;
#[cfg(windows)]
mod window;
