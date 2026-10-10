//! LoopCut GPU compositor (wgpu): color conversion, geometry, grading, LUTs and caption compositing.
//!
//! The same compositor renders both the editor preview and exports, so what you see is what you get.

pub mod captions;
pub mod gpu;
