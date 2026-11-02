//! Everything FormalPacer does apart from drawing: finding games, reading displays, choosing
//! caps. The app and `formalpacer --headless` both drive it through [`engine::start`].

pub mod config;
pub mod display;
pub mod engine;
pub mod layer;
pub mod pacing;
pub mod placement;
pub mod steam;
