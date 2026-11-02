//! The part of FormalPacer that runs inside the game, shared by the Vulkan layer and the OpenGL
//! hook: whether the process is a game, the limiter that holds the cap and records frames, the
//! overlay's picture and the hotkey that toggles it.

pub mod game;
mod hotkey;
pub mod overlay;
pub mod pacer;
