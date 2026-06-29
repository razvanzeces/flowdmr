//! FlowDMR injection entity for FlowStation.
//!
//! Drop-in `TetraEntityTrait` implementation that takes decoded DMR audio from
//! the FlowDMR sidecar and injects it as a LOCAL TETRA group call. See
//! [`entity::FlowDmrEntity`].
//!
//! Integrate by registering it in FlowStation's `bins/bluestation-bs/src/main.rs`
//! (see `integration/flowstation.patch` in the FlowDMR repo).

// Manual bit-width math reads clearer here than `div_ceil`, and stays portable
// across the toolchain pinned for the Pi cross-build.
#![allow(clippy::manual_div_ceil)]

pub mod codec;
pub mod entity;
pub mod jitter;

pub use entity::FlowDmrEntity;
