//! Núcleo de mi-terminal como librería.
//!
//! Los binarios (`mi-terminal`, `collab-broker`) y los tests de integración
//! consumen estos módulos desde acá; ninguna lógica vive en los binarios.

pub mod app;
pub mod canvas;
// Online/invitations are outside this maintenance pass. Keep their existing
// formatting/type-inference behavior while checking the rest with Rust 1.98.
#[allow(clippy::uninlined_format_args, float_literal_f32_fallback)]
pub mod collab;
pub mod command_palette;
pub mod config;
// El daemon usa unix sockets: en Windows la app corre siempre in-process.
#[cfg(unix)]
pub mod daemon;
pub mod memory;
pub mod orchestration;
pub mod panel;
pub mod runtime;
pub mod shortcuts;
pub mod sidebar;
pub mod state;
pub mod terminal;
pub mod theme;
pub mod update;
#[cfg(target_os = "macos")]
pub mod update_install;
pub mod utils;
