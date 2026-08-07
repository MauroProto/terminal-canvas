//! Daemon de PTYs (P3.15): las sesiones viven en un proceso aparte, así
//! cerrar o crashear la app no mata los agentes que están trabajando.

#[cfg(feature = "daemon")]
pub mod backend;
pub mod client;
pub mod protocol;
pub mod server;
#[cfg(feature = "daemon")]
pub mod sessions;
