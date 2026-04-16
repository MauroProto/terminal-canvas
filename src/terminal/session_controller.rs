use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use crate::runtime::{PtyManager, SessionSpec, SharedPtyHandle};
use crate::terminal::input::InputMode;
use crate::terminal::pty::PtyHandle;

#[derive(Default)]
pub struct SessionController {
    pty_manager: Option<Arc<Mutex<PtyManager>>>,
    session_id: Option<Uuid>,
    last_cols: u16,
    last_rows: u16,
    spawn_error: Option<String>,
}

impl SessionController {
    pub fn attach_new_with_spec(
        &mut self,
        pty_manager: Arc<Mutex<PtyManager>>,
        spec: SessionSpec,
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
    ) {
        self.close();
        self.last_cols = cols.max(1);
        self.last_rows = rows.max(1);
        self.pty_manager = Some(Arc::clone(&pty_manager));
        let spawn_result = match pty_manager.lock() {
            Ok(mut manager) => manager.spawn(spec, cwd, self.last_cols, self.last_rows),
            Err(_) => Err(anyhow::anyhow!("PTY manager lock poisoned")),
        };
        match spawn_result {
            Ok(session_id) => {
                self.session_id = Some(session_id);
                self.spawn_error = None;
            }
            Err(err) => {
                self.session_id = None;
                self.spawn_error = Some(err.to_string());
            }
        }
    }

    pub fn restore_detached_with_spec(
        &mut self,
        pty_manager: Arc<Mutex<PtyManager>>,
        spec: SessionSpec,
        cols: u16,
        rows: u16,
    ) {
        self.close();
        self.last_cols = cols.max(1);
        self.last_rows = rows.max(1);
        self.pty_manager = Some(Arc::clone(&pty_manager));
        match pty_manager.lock() {
            Ok(mut manager) => {
                let session_id = manager.create_detached(spec);
                self.session_id = Some(session_id);
                self.spawn_error = None;
            }
            Err(_) => {
                self.session_id = None;
                self.spawn_error = Some("PTY manager lock poisoned".to_owned());
            }
        }
    }

    pub fn runtime_session_id(&self) -> Option<Uuid> {
        self.session_id
    }

    pub fn is_attached(&self) -> bool {
        let Some(manager) = &self.pty_manager else {
            return false;
        };
        let Some(session_id) = self.session_id else {
            return false;
        };
        manager
            .lock()
            .ok()
            .map(|manager| manager.is_attached(session_id))
            .unwrap_or(false)
    }

    pub fn is_alive(&self) -> bool {
        let Some(manager) = &self.pty_manager else {
            return false;
        };
        let Some(session_id) = self.session_id else {
            return false;
        };
        manager
            .lock()
            .ok()
            .map(|manager| manager.is_alive(session_id))
            .unwrap_or(false)
    }

    pub fn ensure_attached(&mut self) -> bool {
        let Some(manager) = &self.pty_manager else {
            return false;
        };
        let Some(session_id) = self.session_id else {
            return false;
        };
        let cols = self.last_cols.max(1);
        let rows = self.last_rows.max(1);
        match manager.lock() {
            Ok(mut manager) => match manager.attach_detached(session_id, cols, rows) {
                Ok(()) => {
                    self.spawn_error = None;
                    true
                }
                Err(err) => {
                    self.spawn_error = Some(err.to_string());
                    false
                }
            },
            Err(_) => {
                self.spawn_error = Some("PTY manager lock poisoned".to_owned());
                false
            }
        }
    }

    pub fn sync_grid_size(&mut self, cols: u16, rows: u16, defer_resize: bool) {
        self.last_cols = cols.max(1);
        self.last_rows = rows.max(1);
        if defer_resize {
            return;
        }
        if !self.is_attached() && !self.ensure_attached() {
            return;
        }
        let _ = self.with_pty_mut(|pty| pty.resize(self.last_cols, self.last_rows));
    }

    pub fn with_pty<R>(&self, f: impl FnOnce(&PtyHandle) -> R) -> Option<R> {
        let handle = self.handle()?;
        let pty = handle.lock().ok()?;
        Some(f(&pty))
    }

    pub fn with_pty_mut<R>(&self, f: impl FnOnce(&mut PtyHandle) -> R) -> Option<R> {
        let handle = self.handle()?;
        let mut pty = handle.lock().ok()?;
        Some(f(&mut pty))
    }

    pub fn input_mode(&self) -> InputMode {
        self.with_pty(PtyHandle::input_mode).unwrap_or_default()
    }

    pub fn selected_text(&self) -> Option<String> {
        self.with_pty(PtyHandle::selected_text).flatten()
    }

    pub fn clear_selection(&self) {
        let _ = self.with_pty(PtyHandle::clear_selection);
    }

    pub fn title_snapshot(&self) -> Option<String> {
        let manager = self.pty_manager.as_ref()?;
        let session_id = self.session_id?;
        manager.lock().ok()?.session_title(session_id)
    }

    pub fn take_bell(&self) -> bool {
        self.with_pty(PtyHandle::take_bell).unwrap_or(false)
    }

    pub fn last_grid_size(&self) -> (u16, u16) {
        (self.last_cols.max(1), self.last_rows.max(1))
    }

    #[cfg(test)]
    pub fn set_last_grid_size_for_tests(&mut self, cols: u16, rows: u16) {
        self.last_cols = cols.max(1);
        self.last_rows = rows.max(1);
    }

    pub fn spawn_error(&self) -> Option<&str> {
        self.spawn_error.as_deref()
    }

    pub fn update_session_title_hint(&self, title: &str) {
        let Some(manager) = &self.pty_manager else {
            return;
        };
        let Some(session_id) = self.session_id else {
            return;
        };
        if let Ok(mut manager) = manager.lock() {
            manager.update_spec_title(session_id, title.to_owned());
        }
    }

    pub fn session_handle(&self) -> Option<SharedPtyHandle> {
        self.handle()
    }

    pub fn close(&mut self) {
        let Some(session_id) = self.session_id.take() else {
            return;
        };
        if let Some(manager) = &self.pty_manager {
            if let Ok(mut manager) = manager.lock() {
                manager.close(session_id);
            }
        }
    }

    fn handle(&self) -> Option<SharedPtyHandle> {
        let manager = self.pty_manager.as_ref()?;
        let session_id = self.session_id?;
        manager.lock().ok()?.handle(session_id)
    }
}

pub fn session_spec(
    title: String,
    cwd: Option<PathBuf>,
    startup_command: Option<String>,
    startup_input: Option<String>,
) -> SessionSpec {
    SessionSpec {
        title,
        cwd,
        startup_command,
        startup_input,
    }
}
