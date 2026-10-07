//! Exportar la salida del terminal enfocado a un archivo de texto en la
//! carpeta de descargas, con aviso (toast) del resultado.

use crate::terminal::export::unique_export_file_name;

use super::TerminalApp;

impl TerminalApp {
    pub(super) fn export_focused_scrollback(&mut self) {
        // Extraemos todo lo que necesitamos del workspace antes de tocar
        // `self` como mutable (los toasts requieren &mut self).
        let snapshot = self
            .ws()
            .focused_panel()
            .map(|panel| (panel.title().to_owned(), panel.scrollback_text()));

        let (title, text) = match snapshot {
            Some((title, Some(text))) => (title, text),
            Some((_, None)) => {
                self.toast_error("Ese panel no tiene un terminal vivo para exportar");
                return;
            }
            None => {
                self.toast_error("No hay ningún terminal enfocado");
                return;
            }
        };
        if text.is_empty() {
            self.toast_error("El terminal no tiene salida todavía");
            return;
        }

        let name = unique_export_file_name(&title, chrono::Local::now());
        let Some(directory) = crate::utils::app_paths::exports_dir() else {
            self.toast_error("No se pudo resolver la carpeta de exportación");
            return;
        };
        let path = directory.join(&name);
        match crate::state::durable_write::write_atomic(&path, text.as_bytes()) {
            Ok(_) => {
                let lines = text.lines().count();
                self.toast_success(format!("{lines} líneas exportadas a {}", path.display()));
            }
            Err(err) => {
                log::warn!(
                    "No se pudo exportar el scrollback a {}: {err}",
                    path.display()
                );
                self.toast_error(format!("No se pudo escribir {}: {err}", path.display()));
            }
        }
    }
}
