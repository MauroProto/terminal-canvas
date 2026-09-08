//! Explorador de archivos del workspace activo (equivalente al file explorer
//! de Orca): árbol perezoso, sólo lee los directorios que el usuario abrió, y
//! al hacer click en un archivo lo abre en el visor interno.
//!
//! El aplanado del árbol y el orden/filtrado de entradas son funciones puras,
//! así que se testean sin tocar el disco (salvo los tests que sí crean un
//! directorio temporal a propósito).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use egui::{Align2, FontId, Sense, Ui};

use super::{SidebarResponse, RAISED, TEXT_MUTED, TEXT_PRIMARY, TEXT_SECONDARY};

/// Directorios que nunca se listan: pesados y sin interés para leer código.
pub const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".terminalcanvas",
    "dist",
    "build",
    ".next",
    "__pycache__",
    ".venv",
    "venv",
];

/// Tope de entradas visibles: un directorio con 50k archivos no puede colgar
/// el sidebar.
const MAX_VISIBLE_ENTRIES: usize = 2_000;
/// Tope de entradas leídas por directorio.
const MAX_ENTRIES_PER_DIR: usize = 1_000;

const ROW_HEIGHT: f32 = 24.0;
const INDENT: f32 = 12.0;
const CONTENT_PAD_X: f32 = 12.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTreeEntry {
    /// Ruta relativa a la raíz del workspace.
    pub rel_path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    /// Profundidad para la indentación (0 = hijos directos de la raíz).
    pub depth: usize,
}

/// Estado del explorador: qué directorios están abiertos y el aplanado
/// cacheado. Se reconstruye sólo cuando cambia la expansión o la raíz.
#[derive(Debug, Default)]
pub struct FileTreeState {
    root: Option<PathBuf>,
    expanded: HashSet<PathBuf>,
    visible: Vec<FileTreeEntry>,
    dirty: bool,
    revision: u64,
    in_flight: Option<FileTreeLoad>,
}

#[derive(Debug)]
struct FileTreeLoad {
    revision: u64,
    receiver: std::sync::mpsc::Receiver<Vec<FileTreeEntry>>,
}

impl FileTreeState {
    /// Apunta el árbol a una raíz nueva. Si es la misma, no descarta la
    /// expansión que el usuario venía armando.
    pub fn set_root(&mut self, root: Option<PathBuf>) {
        if self.root.as_deref() == root.as_deref() {
            return;
        }
        self.root = root;
        self.expanded.clear();
        self.visible.clear();
        self.mark_dirty();
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Fuerza releer el disco en el próximo frame (para el botón de refresh).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }

    fn toggle(&mut self, rel_path: &Path) {
        if self.expanded.contains(rel_path) {
            self.expanded.remove(rel_path);
        } else {
            self.expanded.insert(rel_path.to_path_buf());
        }
        self.mark_dirty();
    }

    fn rebuild_if_needed(&mut self, ctx: &egui::Context) {
        let completed = self
            .in_flight
            .as_ref()
            .map(|job| (job.revision, job.receiver.try_recv()));
        match completed {
            Some((revision, Ok(visible))) => {
                self.in_flight = None;
                if revision == self.revision {
                    self.visible = visible;
                }
            }
            Some((_, Err(std::sync::mpsc::TryRecvError::Disconnected))) => {
                self.in_flight = None;
            }
            Some((_, Err(std::sync::mpsc::TryRecvError::Empty))) | None => {}
        }

        if !self.dirty || self.in_flight.is_some() {
            if self.in_flight.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            return;
        }
        self.dirty = false;
        let Some(root) = self.root.clone() else {
            self.visible.clear();
            return;
        };
        let expanded = self.expanded.clone();
        let revision = self.revision;
        let repaint = ctx.clone();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("sidebar-file-tree".to_owned())
            .spawn(move || {
                let visible = flatten_tree(&root, &expanded);
                let _ = sender.send(visible);
                repaint.request_repaint();
            })
            .is_ok();
        if spawned {
            self.in_flight = Some(FileTreeLoad { revision, receiver });
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

/// Lista el contenido de un directorio ya ordenado: directorios primero y
/// luego archivos, cada grupo alfabético sin distinguir mayúsculas. Filtra los
/// directorios de `SKIP_DIRS`.
pub fn read_dir_sorted(dir: &Path) -> Vec<(String, bool)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut items: Vec<(String, bool)> = Vec::new();
    for entry in entries.flatten().take(MAX_ENTRIES_PER_DIR) {
        let name = entry.file_name().to_string_lossy().to_string();
        // `file_type` no sigue symlinks, así que un symlink a directorio se
        // lista como archivo: preferible a arriesgar un ciclo infinito.
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        if is_dir && SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        items.push((name, is_dir));
    }
    sort_entries(&mut items);
    items
}

/// Directorios antes que archivos; dentro de cada grupo, alfabético
/// case-insensitive con desempate estable por el nombre original.
pub fn sort_entries(items: &mut [(String, bool)]) {
    items.sort_by(|(a_name, a_dir), (b_name, b_dir)| {
        b_dir
            .cmp(a_dir)
            .then_with(|| a_name.to_lowercase().cmp(&b_name.to_lowercase()))
            .then_with(|| a_name.cmp(b_name))
    });
}

/// Aplana el árbol en la lista de filas visibles, descendiendo sólo por los
/// directorios presentes en `expanded`.
pub fn flatten_tree(root: &Path, expanded: &HashSet<PathBuf>) -> Vec<FileTreeEntry> {
    let mut out = Vec::new();
    push_level(root, Path::new(""), 0, expanded, &mut out);
    out
}

fn push_level(
    root: &Path,
    rel_dir: &Path,
    depth: usize,
    expanded: &HashSet<PathBuf>,
    out: &mut Vec<FileTreeEntry>,
) {
    if out.len() >= MAX_VISIBLE_ENTRIES {
        return;
    }
    let abs_dir = root.join(rel_dir);
    for (name, is_dir) in read_dir_sorted(&abs_dir) {
        if out.len() >= MAX_VISIBLE_ENTRIES {
            return;
        }
        let rel_path = if rel_dir.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            rel_dir.join(&name)
        };
        let expanded_here = is_dir && expanded.contains(&rel_path);
        out.push(FileTreeEntry {
            rel_path: rel_path.clone(),
            name,
            is_dir,
            depth,
        });
        if expanded_here {
            push_level(root, &rel_path, depth + 1, expanded, out);
        }
    }
}

/// Dibuja el árbol y devuelve las acciones (abrir archivo / togglear carpeta ya
/// se resuelve internamente).
pub fn draw_file_tree(ui: &mut Ui, state: &mut FileTreeState) -> Vec<SidebarResponse> {
    let mut responses = Vec::new();

    let Some(root) = state.root.clone() else {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(CONTENT_PAD_X);
            ui.label(
                egui::RichText::new("Abrí una carpeta para ver sus archivos")
                    .size(11.5)
                    .color(TEXT_MUTED),
            );
        });
        return responses;
    };

    state.rebuild_if_needed(ui.ctx());

    // Encabezado con el nombre de la carpeta y un refresh.
    let header = root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| root.display().to_string());
    let refresh_width = 62.0;
    let (header_rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(60.0), 28.0),
        Sense::hover(),
    );
    let refresh_rect = egui::Rect::from_min_size(
        egui::pos2(
            header_rect.right() - CONTENT_PAD_X - refresh_width,
            header_rect.top(),
        ),
        egui::vec2(refresh_width, 28.0),
    );
    let label_rect = egui::Rect::from_min_max(
        egui::pos2(header_rect.left() + CONTENT_PAD_X, header_rect.top()),
        egui::pos2(refresh_rect.left() - 8.0, header_rect.bottom()),
    );
    ui.put(
        label_rect,
        egui::Label::new(
            egui::RichText::new(header)
                .size(12.0)
                .color(TEXT_PRIMARY)
                .strong(),
        )
        .truncate(),
    );
    if ui
        .put(
            refresh_rect,
            egui::Button::new(egui::RichText::new("Refresh").size(11.0)),
        )
        .on_hover_text("Releer del disco")
        .clicked()
    {
        state.mark_dirty();
    }
    ui.add_space(4.0);

    if state.visible.is_empty() {
        let label = if state.in_flight.is_some() {
            "(leyendo carpeta…)"
        } else {
            "(carpeta vacía)"
        };
        ui.horizontal(|ui| {
            ui.add_space(CONTENT_PAD_X);
            ui.label(egui::RichText::new(label).size(11.5).color(TEXT_MUTED));
        });
        return responses;
    }

    let width = ui.available_width();
    let mut toggle: Option<PathBuf> = None;
    for entry in &state.visible {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width.max(60.0), ROW_HEIGHT), Sense::click());
        response.widget_info(|| {
            let action = if entry.is_dir {
                "Abrir carpeta"
            } else {
                "Abrir archivo"
            };
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                format!("{action} {}", entry.name),
            )
        });
        if response.hovered() {
            ui.painter()
                .rect_filled(rect.shrink2(egui::vec2(2.0, 1.0)), 4.0, RAISED);
        }
        let color = if response.hovered() || entry.is_dir {
            TEXT_PRIMARY
        } else {
            TEXT_SECONDARY
        };
        let x = rect.left() + CONTENT_PAD_X + entry.depth as f32 * INDENT;
        let marker = if entry.is_dir {
            if state.expanded.contains(&entry.rel_path) {
                "▾"
            } else {
                "▸"
            }
        } else {
            " "
        };
        ui.painter().text(
            egui::pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            marker,
            FontId::proportional(10.0),
            color,
        );
        ui.painter().text(
            egui::pos2(x + 12.0, rect.center().y),
            Align2::LEFT_CENTER,
            &entry.name,
            FontId::proportional(12.0),
            color,
        );
        if response.clicked() {
            if entry.is_dir {
                toggle = Some(entry.rel_path.clone());
            } else {
                responses.push(SidebarResponse::OpenFileInViewer(
                    root.join(&entry.rel_path),
                ));
            }
        }
    }
    if let Some(path) = toggle {
        state.toggle(&path);
    }

    responses
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    use super::{flatten_tree, read_dir_sorted, sort_entries, FileTreeState, SKIP_DIRS};

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("file-tree-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create root");
        root
    }

    #[test]
    fn directories_sort_before_files() {
        let mut items = vec![
            ("zeta.rs".to_owned(), false),
            ("alpha".to_owned(), true),
            ("beta.rs".to_owned(), false),
            ("omega".to_owned(), true),
        ];
        sort_entries(&mut items);
        let names: Vec<&str> = items.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "omega", "beta.rs", "zeta.rs"]);
    }

    #[test]
    fn sorting_is_case_insensitive() {
        let mut items = vec![
            ("Zebra.rs".to_owned(), false),
            ("apple.rs".to_owned(), false),
            ("Banana.rs".to_owned(), false),
        ];
        sort_entries(&mut items);
        let names: Vec<&str> = items.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["apple.rs", "Banana.rs", "Zebra.rs"]);
    }

    #[test]
    fn heavy_directories_are_never_listed() {
        let root = temp_root("skip");
        std::fs::create_dir_all(root.join("node_modules/pkg")).expect("mkdir");
        std::fs::create_dir_all(root.join(".git/objects")).expect("mkdir");
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        std::fs::write(root.join("Cargo.toml"), b"x").expect("write");

        let listed = read_dir_sorted(&root);
        let _ = std::fs::remove_dir_all(&root);

        let names: Vec<&str> = listed.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["src", "Cargo.toml"]);
        for skipped in SKIP_DIRS {
            assert!(!names.contains(skipped), "{skipped} leaked into the tree");
        }
    }

    #[test]
    fn collapsed_root_only_lists_the_first_level() {
        let root = temp_root("collapsed");
        std::fs::create_dir_all(root.join("src/deep")).expect("mkdir");
        std::fs::write(root.join("src/main.rs"), b"x").expect("write");
        std::fs::write(root.join("README.md"), b"x").expect("write");

        let flat = flatten_tree(&root, &HashSet::new());
        let _ = std::fs::remove_dir_all(&root);

        let names: Vec<&str> = flat.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["src", "README.md"]);
        assert!(flat.iter().all(|entry| entry.depth == 0));
    }

    #[test]
    fn expanding_a_directory_inlines_its_children_with_deeper_indent() {
        let root = temp_root("expanded");
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        std::fs::write(root.join("src/main.rs"), b"x").expect("write");
        std::fs::write(root.join("src/lib.rs"), b"x").expect("write");
        std::fs::write(root.join("README.md"), b"x").expect("write");

        let mut expanded = HashSet::new();
        expanded.insert(PathBuf::from("src"));
        let flat = flatten_tree(&root, &expanded);
        let _ = std::fs::remove_dir_all(&root);

        let rows: Vec<(&str, usize)> = flat
            .iter()
            .map(|entry| (entry.name.as_str(), entry.depth))
            .collect();
        // src abierto: sus hijos van pegados debajo, con depth 1.
        assert_eq!(
            rows,
            vec![("src", 0), ("lib.rs", 1), ("main.rs", 1), ("README.md", 0)]
        );
    }

    #[test]
    fn expanded_paths_are_relative_so_nested_names_do_not_collide() {
        let root = temp_root("nested");
        std::fs::create_dir_all(root.join("a/src")).expect("mkdir");
        std::fs::create_dir_all(root.join("b/src")).expect("mkdir");
        std::fs::write(root.join("a/src/only-in-a.rs"), b"x").expect("write");
        std::fs::write(root.join("b/src/only-in-b.rs"), b"x").expect("write");

        // Abrimos sólo a/src: b/src debe seguir cerrado aunque se llame igual.
        let mut expanded = HashSet::new();
        expanded.insert(PathBuf::from("a"));
        expanded.insert(PathBuf::from("a/src"));
        let flat = flatten_tree(&root, &expanded);
        let _ = std::fs::remove_dir_all(&root);

        let names: Vec<&str> = flat.iter().map(|entry| entry.name.as_str()).collect();
        assert!(names.contains(&"only-in-a.rs"), "got {names:?}");
        assert!(
            !names.contains(&"only-in-b.rs"),
            "b/src must stay collapsed, got {names:?}"
        );
    }

    #[test]
    fn missing_directory_yields_no_entries_instead_of_panicking() {
        let missing = std::env::temp_dir().join(format!("nope-{}", uuid::Uuid::new_v4()));
        assert!(read_dir_sorted(&missing).is_empty());
        assert!(flatten_tree(&missing, &HashSet::new()).is_empty());
    }

    #[test]
    fn changing_root_resets_expansion_but_same_root_keeps_it() {
        let mut state = FileTreeState::default();
        state.set_root(Some(PathBuf::from("/tmp/a")));
        state.toggle(Path::new("src"));
        assert!(state.expanded.contains(Path::new("src")));

        // Misma raíz: no se pierde lo que el usuario abrió.
        state.set_root(Some(PathBuf::from("/tmp/a")));
        assert!(state.expanded.contains(Path::new("src")));

        // Raíz distinta: la expansión anterior no aplica.
        state.set_root(Some(PathBuf::from("/tmp/b")));
        assert!(state.expanded.is_empty());
    }

    #[test]
    fn toggle_opens_and_closes() {
        let mut state = FileTreeState::default();
        state.set_root(Some(PathBuf::from("/tmp/a")));
        state.toggle(Path::new("src"));
        assert!(state.expanded.contains(Path::new("src")));
        state.toggle(Path::new("src"));
        assert!(!state.expanded.contains(Path::new("src")));
    }
}
