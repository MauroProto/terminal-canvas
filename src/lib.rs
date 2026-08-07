//! Núcleo de mi-terminal como librería.
//!
//! Los binarios (`mi-terminal`, `collab-broker`) y los tests de integración
//! consumen estos módulos desde acá; ninguna lógica vive en los binarios.

pub mod app;
pub mod canvas;
pub mod collab;
pub mod command_palette;
pub mod config;
// El daemon usa unix sockets: en Windows la app corre siempre in-process.
#[cfg(unix)]
pub mod daemon;
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
