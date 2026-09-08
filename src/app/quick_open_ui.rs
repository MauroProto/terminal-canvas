//! Quick Open: búsqueda fuzzy de archivos del workspace activo (estilo IDE).
//! Enter abre el archivo con la aplicación default del SO.

use std::path::{Path, PathBuf};

use egui::{pos2, vec2, Align2, FontId, RichText, ScrollArea, Sense};

use crate::command_palette::commands::COMMANDS;
use crate::command_palette::rank::{
    clamp_query, rank_command, rank_file, rank_panel, top_k, QuickOpenKind, RankClass, RankedItem,
    TOP_K,
};
use crate::theme::colors as palette;

use super::TerminalApp;

const MAX_FILES: usize = 10_000;
const MAX_VISITED: usize = 60_000;
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".terminalcanvas",
    ".terminalcanvas-archive",
    ".terminalcanvas-trash",
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
/// directorios pesados. Los archivos de configuración ocultos son buscables.
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
                if SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                stack.push(rel_child);
            } else {
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
        let mut chosen: Option<RankedItem> = None;

        egui::Area::new(egui::Id::new("quick-open"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 70.0))
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(palette::INK)
                    .stroke(egui::Stroke::new(1.0, palette::LINE))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_min_width(460.0);
                        ui.label(
                            RichText::new("Quick Open — archivos, > comandos, @ paneles")
                                .size(12.0)
                                .color(palette::DIM),
                        );
                        ui.add_space(6.0);
                        let edit = egui::TextEdit::singleline(
                            &mut self.quick_open.as_mut().unwrap().query,
                        )
                        .hint_text("archivo · > comando · @ panel")
                        .text_color(palette::TEXT_STRONG)
                        .margin(egui::Margin::symmetric(10, 6));
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
                        let results: Vec<RankedItem> = {
                            let query = self.quick_open.as_ref().unwrap().query.clone();
                            let files = &self.quick_open.as_ref().unwrap().files;
                            let panels = self.quick_open_panels();
                            unified_results(&query, files, &panels)
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
                            if let Some(item) =
                                results.get(selected.min(results.len().saturating_sub(1)))
                            {
                                chosen = Some(item.clone());
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
                            self.quick_open_results(ui, &results, &mut chosen, &mut close);
                        }
                    });
            });

        if let Some(item) = chosen {
            let root = self
                .quick_open
                .as_ref()
                .map(|state| state.root.clone())
                .unwrap_or_default();
            match item.kind {
                QuickOpenKind::File => open_file = Some(root.join(&item.label)),
                QuickOpenKind::Panel(panel_id) => {
                    self.focus_panel_across_workspaces(panel_id, Some(ctx.available_rect()));
                }
                QuickOpenKind::Command => {
                    if let Some(entry) = COMMANDS
                        .iter()
                        .find(|entry| entry.label == item.label)
                        .copied()
                    {
                        let canvas_rect = ctx.available_rect();
                        self.quick_open = None;
                        self.execute_command(entry.command, ctx, canvas_rect);
                    }
                }
            }
        }
        if let Some(path) = open_file {
            // Abrí el visor in-app; el editor externo queda como botón ahí.
            self.open_file_viewer(path);
        }
        if close {
            self.quick_open = None;
        }
    }

    /// Paneles del workspace activo ordenados por recencia de foco (el
    /// z_index más alto es el que se tocó último).
    pub(super) fn quick_open_panels(&self) -> Vec<(uuid::Uuid, String)> {
        let mut panels: Vec<(uuid::Uuid, String, u32)> = self
            .ws()
            .panels
            .iter()
            .map(|panel| (panel.id(), panel.title().to_owned(), panel.z_index()))
            .collect();
        panels.sort_by(|a, b| b.2.cmp(&a.2));
        panels
            .into_iter()
            .map(|(id, title, _)| (id, title))
            .collect()
    }

    fn quick_open_results(
        &mut self,
        ui: &mut egui::Ui,
        results: &[RankedItem],
        chosen: &mut Option<RankedItem>,
        close: &mut bool,
    ) {
        let selected = self.quick_open.as_ref().unwrap().selected;

        if results.is_empty() {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Sin coincidencias")
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
                for (index, item) in results.iter().enumerate() {
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
                    // Un glifo por tipo: se ve de un vistazo si es comando,
                    // panel o archivo.
                    let glyph = match item.kind {
                        QuickOpenKind::Command => "\u{203a}",
                        QuickOpenKind::Panel(_) => "@",
                        QuickOpenKind::File => " ",
                    };
                    ui.painter().text(
                        pos2(rect.left() + 10.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        glyph,
                        FontId::monospace(11.5),
                        palette::DIM,
                    );
                    ui.painter().text(
                        pos2(rect.left() + 26.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        item.label.as_str(),
                        FontId::monospace(11.5),
                        color,
                    );
                    if response.clicked() {
                        *chosen = Some(item.clone());
                        *close = true;
                    }
                }
            });
    }
}

/// Fuentes mezcladas del quick open (P2.14, T2). Los prefijos deciden la
/// fuente: `>` comandos, `@` paneles/agentes, sin prefijo archivos (más los
/// comandos que matcheen exacto, para que "New Terminal" siga siendo tipeable
/// sin prefijo). Query vacía = paneles por recencia de foco.
pub(super) fn unified_results(
    query: &str,
    files: &[String],
    panels: &[(uuid::Uuid, String)],
) -> Vec<RankedItem> {
    let query = clamp_query(query).trim();

    // Query vacía: los paneles, en el orden de recencia que ya viene dado.
    if query.is_empty() {
        return panels
            .iter()
            .enumerate()
            .map(|(index, (panel_id, title))| RankedItem {
                class: RankClass::PanelName,
                // El primero de la lista es el más reciente.
                score: (panels.len() - index) as i32,
                label: title.clone(),
                kind: QuickOpenKind::Panel(*panel_id),
            })
            .collect();
    }

    if let Some(rest) = query.strip_prefix('>') {
        let rest = rest.trim();
        return top_k(
            COMMANDS
                .iter()
                .filter(|entry| entry.command.available_on_desktop())
                .filter_map(|entry| rank_command(rest, entry.label)),
            TOP_K,
        );
    }
    if let Some(rest) = query.strip_prefix('@') {
        let rest = rest.trim();
        return top_k(
            panels
                .iter()
                .filter_map(|(panel_id, title)| rank_panel(rest, title, *panel_id)),
            TOP_K,
        );
    }

    // Sin prefijo: archivos, más los comandos que matcheen exacto o por
    // prefijo (las reglas ordinales los ponen arriba solos).
    let commands = COMMANDS
        .iter()
        .filter(|entry| entry.command.available_on_desktop())
        .filter_map(|entry| {
            rank_command(query, entry.label).filter(|item| {
                matches!(
                    item.class,
                    RankClass::ExactCommand | RankClass::CommandPrefix
                )
            })
        });
    let files = files.iter().filter_map(|path| rank_file(query, path));
    top_k(commands.chain(files), TOP_K)
}

#[cfg(test)]
mod tests {
    use super::{collect_files, unified_results};
    use crate::command_palette::rank::QuickOpenKind;

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
        std::fs::write(dir.join(".gitignore"), "target/").unwrap();
        std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
        std::fs::write(dir.join(".github/workflows/test.yml"), "name: test").unwrap();
        std::fs::create_dir_all(dir.join(".terminalcanvas-archive/old")).unwrap();
        std::fs::write(dir.join(".terminalcanvas-archive/old/main.rs"), "old").unwrap();

        let files = collect_files(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(files.contains(&".gitignore".to_owned()));
        assert!(files.contains(&".github/workflows/test.yml".to_owned()));
        assert!(!files
            .iter()
            .any(|path| path.starts_with(".terminalcanvas-archive/")));

        assert!(files.contains(&"src/main.rs".to_owned()));
        assert!(files.contains(&"README.md".to_owned()));
        assert!(!files.iter().any(|f| f.starts_with("node_modules")));
        assert!(!files.iter().any(|f| f.starts_with(".git/")));
    }

    fn sample_files() -> Vec<String> {
        vec![
            "src/main.rs".to_owned(),
            "src/lib.rs".to_owned(),
            "README.md".to_owned(),
        ]
    }

    fn sample_panels() -> Vec<(uuid::Uuid, String)> {
        vec![
            (uuid::Uuid::new_v4(), "claude code".to_owned()),
            (uuid::Uuid::new_v4(), "shell".to_owned()),
        ]
    }

    #[test]
    fn no_prefix_searches_files() {
        let results = unified_results("main", &sample_files(), &sample_panels());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].label, "src/main.rs");
        assert_eq!(results[0].kind, QuickOpenKind::File);
    }

    #[test]
    fn the_command_prefix_only_lists_commands() {
        let results = unified_results("> new", &sample_files(), &sample_panels());
        assert!(!results.is_empty());
        assert!(
            results
                .iter()
                .all(|item| item.kind == QuickOpenKind::Command),
            "solo comandos"
        );
        assert!(results.iter().any(|item| item.label == "New Terminal"));
    }

    #[test]
    fn the_at_prefix_only_lists_panels() {
        let panels = sample_panels();
        let results = unified_results("@claude", &sample_files(), &panels);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].label, "claude code");
        assert!(matches!(results[0].kind, QuickOpenKind::Panel(_)));
    }

    #[test]
    fn an_exact_command_shows_up_without_a_prefix() {
        let results = unified_results("New Terminal", &sample_files(), &sample_panels());
        assert_eq!(
            results[0].label, "New Terminal",
            "el comando exacto va primero"
        );
        assert_eq!(results[0].kind, QuickOpenKind::Command);
    }

    #[test]
    fn an_empty_query_lists_panels_by_recency() {
        let panels = sample_panels();
        let results = unified_results("", &sample_files(), &panels);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].label, "claude code", "el más reciente primero");
        assert!(results
            .iter()
            .all(|item| matches!(item.kind, QuickOpenKind::Panel(_))));
    }

    #[test]
    fn a_query_with_no_matches_is_empty() {
        let results = unified_results("zzzzz", &sample_files(), &sample_panels());
        assert!(results.is_empty());
    }
}
