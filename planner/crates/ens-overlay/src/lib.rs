//! Vulkan layer that draws the damage meter into Enshrouded's frames (docs/plans/game-overlay.md).

pub mod draw;
pub mod frame;
pub mod input;
pub mod install;
#[cfg(windows)]
pub mod layer;
