#[cfg(target_arch = "wasm32")]
mod wasm_app;

#[cfg(target_arch = "wasm32")]
pub use wasm_app::*;

#[cfg(not(target_arch = "wasm32"))]
mod template;

#[cfg(not(target_arch = "wasm32"))]
pub use template::{render_docs, render_index, render_map};

#[cfg(not(target_arch = "wasm32"))]
pub fn wasm_placeholder() {}
