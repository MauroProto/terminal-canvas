//! Quick Open: búsqueda fuzzy de archivos del workspace activo (estilo IDE).
//! Enter abre el archivo con la aplicación default del SO.

use std::path::{Path, PathBuf};

use egui::{pos2, vec2, Align2, FontId, RichText, ScrollArea, Sense};

use crate::command_palette::fuzzy::fuzzy_score;
use crate::theme::colors as palette;

use super::TerminalApp;

const MAX_FILES: usize = 10_000;
const MAX_VISITED: usize = 60_000;
const MAX_RESULTS: usize = 50;
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".terminalcanvas",
    "dist",
    "build",
    ".next",
    "__pycache__",
    ".venv",
    "venv",
];

pub(super) struct QuickOpenState {
    pub(super) query: String,
    pub(super) root: PathBuf,
    pub(super) files: Vec<String>,
    pub(super) selected: usize,
    pub(super) loading: bool,
}

/// Junta los archivos del workspace (relativos a `root`), salteando
/// directorios pesados y ocultos. Limitado para no bloquear en repos enormes.
pub(super) fn collect_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut visited = 0usize;
    let mut stack = vec![PathBuf::from(".")];
    while let Some(rel) = stack.pop() {
        if visited >= MAX_VISITED || files.len() >= MAX_FILES {
            break;
        }
        let abs = root.join(&rel);
        let Ok(entries) = std::fs::read_dir(&abs) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited >= MAX_VISITED {
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let rel_child = if rel == Path::new(".") {
                PathBuf::from(&name)
            } else {
                rel.join(&name)
            };
            let file_type = entry.file_type();
            let is_dir = file_type.as_ref().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                stack.push(rel_child);
            } else if !name.starts_with('.') {
                let rel_str = rel_child.to_string_lossy().replace('\\', "/");
                files.push(rel_str);
                if files.len() >= MAX_FILES {
                    break;
                }
            }
        }
    }
    files.sort();
    files
}

/// Filtra y ordena los archivos por score fuzzy. Con query vacía devuelve los
/// primeros (orden alfabético).
pub(super) fn match_files<'a>(query: &str, files: &'a [String]) -> Vec<&'a String> {
    let query = query.trim();
    if query.is_empty() {
        return files.iter().take(MAX_RESULTS).collect();
    }
    let mut scored: Vec<(i32, &String)> = files
        .iter()
        .filter_map(|file| fuzzy_score(query, file).map(|score| (score, file)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, file)| file)
        .collect()
}

impl TerminalApp {
    pub(super) fn open_quick_open(&mut self) {
        let Some(cwd) = self.ws().cwd.clone() else {
            return;
        };
        // El walk corre en un worker para no bloquear la UI en repos grandes.
        let (tx, rx) = std::sync::mpsc::channel::<Vec<String>>();
        let walk_root = cwd.clone();
        let _ = std::thread::Builder::new()
            .name("quick-open-walk".to_owned())
            .spawn(move || {
                let files = collect_files(&walk_root);
                let _ = tx.send(files);
            });
        self.quick_open_rx = Some(rx);
        self.quick_open = Some(QuickOpenState {
            query: String::new(),
            root: cwd,
            files: Vec::new(),
            selected: 0,
            loading: true,
        });
    }

    /// Drena el resultado del walk de archivos cuando llega.
    pub(super) fn poll_quick_open(&mut self) {
        let Some(rx) = self.quick_open_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(files) => {
                if let Some(state) = self.quick_open.as_mut() {
                    state.files = files;
                    state.loading = false;
                }
                self.quick_open_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // El worker terminó sin enviar nada (spawn falló): marcar
                // cargado para no quedar en "Cargando…" eterno.
                if let Some(state) = self.quick_open.as_mut() {
                    state.loading = false;
                }
                self.quick_open_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    pub(super) fn show_quick_open(&mut self, ctx: &egui::Context) {
        if self.quick_open.is_none() {
            return;
        }

        let mut close = false;
        let mut open_file: Option<PathBuf> = None;

        egui::Area::new(egui::Id::new("quick-open"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 70.0))
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(palette::INK)
                    .stroke(egui::Stroke::new(1.0, palette::LINE))
                    .rounding(10.0)
                    .inner_margin(egui::Margin::same(14.0))
                    .show(ui, |ui| {
                        ui.set_min_width(460.0);
                        ui.label(
                            RichText::new("Quick Open — buscar archivo")
                                .size(12.0)
                                .color(palette::DIM),
                        );
                        ui.add_space(6.0);
                        let edit = egui::TextEdit::singleline(
                            &mut self.quick_open.as_mut().unwrap().query,
                        )
                        .hint_text("Escribí para filtrar archivos…")
                        .text_color(palette::TEXT_STRONG)
                        .margin(egui::Margin::symmetric(10.0, 6.0));
                        let response = ui.add_sized(vec2(432.0, 30.0), edit);
                        if response.changed() {
                            self.quick_open.as_mut().unwrap().selected = 0;
                        }
                        if !response.has_focus() && ctx.memory(|memory| memory.focused()).is_none()
                        {
                            response.request_focus();
                        }

                        // Los matches se computan una sola vez por frame
                        // (el fuzzy es O(archivos); no repetir 3 veces).
                        let results: Vec<String> = {
                            let state = self.quick_open.as_ref().unwrap();
                            match_files(&state.query, &state.files)
                                .into_iter()
                                .cloned()
                                .collect()
                        };

                        // Navegación por teclado.
                        let mut selected = self.quick_open.as_ref().unwrap().selected;
                        if ctx.input(|input| input.key_pressed(egui::Key::ArrowDown)) {
                            selected = (selected + 1).min(results.len().saturating_sub(1));
                        }
                        if ctx.input(|input| input.key_pressed(egui::Key::ArrowUp)) {
                            selected = selected.saturating_sub(1);
                        }
                        self.quick_open.as_mut().unwrap().selected = selected;

                        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
                            close = true;
                        }
                        if ctx.input(|input| input.key_pressed(egui::Key::Enter)) {
                            let root = self.quick_open.as_ref().unwrap().root.clone();
                            if let Some(file) =
                                results.get(selected.min(results.len().saturating_sub(1)))
                            {
                                open_file = Some(root.join(file));
                                close = true;
                            }
                        }

                        ui.add_space(6.0);
                        ui.separator();
                        let loading = self.quick_open.as_ref().is_some_and(|state| state.loading);
                        if loading {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("Cargando archivos…")
                                    .size(11.5)
                                    .color(palette::DIM),
                            );
                        } else {
                            self.quick_open_results(ui, &results, &mut open_file, &mut close);
                        }
                    });
            });

        if let Some(path) = open_file {
            // Abrí el visor in-app; el editor externo queda como botón ahí.
            self.open_file_viewer(path);
        }
        if close {
            self.quick_open = None;
        }
    }

    fn quick_open_results(
        &mut self,
        ui: &mut egui::Ui,
        results: &[String],
        open_file: &mut Option<PathBuf>,
        close: &mut bool,
    ) {
        let (root, selected) = {
            let state = self.quick_open.as_ref().unwrap();
            (state.root.clone(), state.selected)
        };

        if results.is_empty() {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Sin archivos coincidentes")
                    .size(11.5)
                    .color(palette::DIM),
            );
            return;
        }

        ScrollArea::vertical()
            .id_salt("quick-open-results")
            .max_height(320.0)
            .show(ui, |ui| {
                ui.set_min_width(432.0);
                for (index, file) in results.iter().enumerate() {
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(432.0, 26.0), Sense::click());
                    if index == selected {
                        ui.painter().rect_filled(rect, 4.0, palette::FOCUS);
                    } else if response.hovered() {
                        ui.painter().rect_filled(rect, 4.0, palette::HOVER);
                    }
                    let color = if index == selected {
                        palette::TEXT_STRONG
                    } else {
                        palette::TEXT
                    };
                    ui.painter().text(
                        pos2(rect.left() + 10.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        file.as_str(),
                        FontId::monospace(11.5),
                        color,
                    );
                    if response.clicked() {
                        *open_file = Some(root.join(file));
                        *close = true;
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::{collect_files, match_files};

    #[test]
    fn collect_files_walks_and_skips_heavy_dirs() {
        let dir = std::env::temp_dir().join(format!("quick-open-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("README.md"), "# hi").unwrap();
        std::fs::write(dir.join("node_modules/pkg/index.js"), "x").unwrap();
        std::fs::write(dir.join(".git/config"), "x").unwrap();

        let files = collect_files(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(files.contains(&"src/main.rs".to_owned()));
        assert!(files.contains(&"README.md".to_owned()));
        assert!(!files.iter().any(|f| f.starts_with("node_modules")));
        assert!(!files.iter().any(|f| f.starts_with(".git")));
    }

    #[test]
    fn match_files_filters_and_ranks() {
        let files = vec![
            "src/main.rs".to_owned(),
            "src/lib.rs".to_owned(),
            "README.md".to_owned(),
        ];
        let results = match_files("main", &files);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], "src/main.rs");

        let all = match_files("", &files);
        assert_eq!(all.len(), 3);

        let none = match_files("zzz", &files);
        assert!(none.is_empty());
    }
}
