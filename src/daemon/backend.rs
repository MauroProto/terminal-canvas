//! Adopción del daemon desde la app (P3.15, T3).
//!
//! Detrás del feature `daemon`. Si el daemon no está, se lo levanta; si no
//! arranca, `DaemonBackend::Fallback` y la app sigue in-process: el daemon es
//! una mejora, no un requisito.
//!
//! El render de los grids sigue siendo in-process. Lo que este backend aporta
//! hoy es el ciclo de vida: adoptar el daemon, reconciliar sesiones huérfanas
//! y pedirle que se apague cuando ya no queda nadie.

use std::path::PathBuf;

use uuid::Uuid;

use super::client::{daemon_binary_path, DaemonConn};
use super::protocol::{ensure_token, Request};
use super::server::resolve_dir;

/// Estado de la adopción del daemon.
pub enum DaemonBackend {
    /// Conectado: el daemon está vivo y adoptado.
    Connected { conn: DaemonConn, dir: PathBuf },
    /// Sin daemon: la app corre las sesiones ella misma.
    Fallback { reason: String },
}

impl DaemonBackend {
    /// Intenta adoptar el daemon. Nunca falla: peor caso, `Fallback`.
    pub fn adopt() -> Self {
        let Some(dir) = resolve_dir() else {
            return Self::fallback("no se pudo resolver el directorio del daemon");
        };
        let token = match ensure_token(&dir) {
            Ok(token) => token,
            Err(err) => return Self::fallback(format!("no se pudo preparar el token: {err}")),
        };
        let Some(binary) = daemon_binary_path() else {
            return Self::fallback("no se encontró el binario del daemon");
        };
        match DaemonConn::connect(&dir, &token, &binary) {
            Some(conn) => Self::Connected { conn, dir },
            None => Self::fallback("el daemon no arrancó"),
        }
    }

    fn fallback(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        log::info!("daemon no adoptado ({reason}): la app corre las sesiones in-process");
        Self::Fallback { reason }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }

    /// Endpoint para abrir conexiones nuevas (una por sesión remota).
    pub fn endpoint(&self) -> Option<super::sessions::DaemonEndpoint> {
        match self {
            Self::Connected { dir, .. } => super::protocol::ensure_token(dir)
                .ok()
                .map(|token| super::sessions::DaemonEndpoint::new(dir.clone(), token)),
            Self::Fallback { .. } => None,
        }
    }

    /// Motivo del fallback, si aplica (para el diagnóstico exportable).
    pub fn fallback_reason(&self) -> Option<&str> {
        match self {
            Self::Fallback { reason } => Some(reason),
            Self::Connected { .. } => None,
        }
    }

    /// Le dice al daemon qué paneles siguen vivos; devuelve los huérfanos que
    /// mató. Sin daemon no hay nada que reconciliar.
    pub fn reconcile_live(&mut self, live: Vec<Uuid>) -> Vec<Uuid> {
        match self {
            Self::Connected { conn, .. } => conn.reconcile_live(live),
            Self::Fallback { .. } => Vec::new(),
        }
    }

    /// Al cerrar la última app: que se apague si no le queda nada.
    pub fn shutdown_if_idle(&mut self) {
        if let Self::Connected { conn, .. } = self {
            let _ = conn.request(&Request::ShutdownIfIdle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DaemonBackend;
    use uuid::Uuid;

    #[test]
    fn the_fallback_explains_itself_and_does_nothing() {
        let mut backend = DaemonBackend::Fallback {
            reason: "el daemon no arrancó".to_owned(),
        };
        assert!(!backend.is_connected());
        assert_eq!(backend.fallback_reason(), Some("el daemon no arrancó"));
        // Sin daemon, reconciliar no puede matar nada.
        assert!(backend.reconcile_live(vec![Uuid::new_v4()]).is_empty());
        // Y pedir el apagado es un no-op, no un panic.
        backend.shutdown_if_idle();
    }

    #[test]
    fn adopting_without_a_daemon_binary_falls_back_instead_of_failing() {
        // En el entorno de test el binario puede no estar construido: lo que
        // importa es que `adopt` nunca paniquee ni cuelgue la app.
        let backend = DaemonBackend::adopt();
        if !backend.is_connected() {
            assert!(backend.fallback_reason().is_some());
        }
    }
}
