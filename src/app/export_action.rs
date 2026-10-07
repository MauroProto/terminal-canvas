//! Exportar la salida del terminal enfocado a un archivo de texto en la
//! carpeta de descargas, con aviso (toast) del resultado.

use crate::terminal::export::unique_export_file_name;

use super::TerminalApp;

impl TerminalApp {
    pub(super) fn export_focused_scrollback(&mut self) {
        if self.export_worker.busy() {
            self.toast_error("Ya hay una exportación en curso; esperá a que termine");
            return;
        }
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
        let lines = text.lines().count();
        self.submit_export(super::export_worker::Job::Text { path, text, lines });
    }

    pub(super) fn submit_export(&mut self, job: super::export_worker::Job) {
        match self.export_worker.try_submit(job) {
            Ok(()) => {
                self.toast_success("Exportación en curso");
                if let Some(ctx) = &self.ctx {
                    ctx.request_repaint();
                }
            }
            Err(error) => self.toast_error(error.to_string()),
        }
    }

    pub(super) fn poll_exports(&mut self, ctx: &egui::Context) {
        if let Some(completion) = self.export_worker.poll() {
            self.finish_export(completion);
        }
        if self.export_worker.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }
    }

    pub(super) fn finish_export(&mut self, completion: super::export_worker::Completion) {
        match (completion.kind, completion.result) {
            (super::export_worker::Kind::Text { lines }, Ok(path)) => {
                self.toast_success(format!("{lines} líneas exportadas a {}", path.display()));
            }
            (super::export_worker::Kind::Diagnostics, Ok(path)) => {
                self.toast_success(format!("Diagnóstico en {}", path.display()));
            }
            (_, Err(error)) => self.toast_error(format!("No se pudo exportar: {error}")),
        }
    }
}
