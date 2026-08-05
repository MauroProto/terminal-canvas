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

const ROW_HEIGHT: f32 = 20.0;
const INDENT: f32 = 12.0;

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
        self.dirty = true;
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Fuerza releer el disco en el próximo frame (para el botón de refresh).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn toggle(&mut self, rel_path: &Path) {
        if self.expanded.contains(rel_path) {
            self.expanded.remove(rel_path);
        } else {
            self.expanded.insert(rel_path.to_path_buf());
        }
        self.dirty = true;
    }

    fn rebuild_if_needed(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.visible = match self.root.as_deref() {
            Some(root) => flatten_tree(root, &self.expanded),
            None => Vec::new(),
        };
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
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Abrí una carpeta para ver sus archivos")
                .size(11.0)
                .color(TEXT_MUTED),
        );
        return responses;
    };

    state.rebuild_if_needed();

    // Encabezado con el nombre de la carpeta y un refresh.
    let header = root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| root.display().to_string());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(header).size(11.0).color(TEXT_SECONDARY));
        if ui
            .small_button("↻")
            .on_hover_text("Releer del disco")
            .clicked()
        {
            state.mark_dirty();
        }
    });
    ui.add_space(2.0);

    if state.visible.is_empty() {
        ui.label(
            egui::RichText::new("(carpeta vacía)")
                .size(11.0)
                .color(TEXT_MUTED),
        );
        return responses;
    }

    let width = ui.available_width();
    let mut toggle: Option<PathBuf> = None;
    for entry in &state.visible {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width.max(60.0), ROW_HEIGHT), Sense::click());
        if response.hovered() {
            ui.painter()
                .rect_filled(rect.shrink2(egui::vec2(2.0, 1.0)), 4.0, RAISED);
        }
        let color = if response.hovered() {
            TEXT_PRIMARY
        } else if entry.is_dir {
            TEXT_SECONDARY
        } else {
            TEXT_MUTED
        };
        let x = rect.left() + 8.0 + entry.depth as f32 * INDENT;
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
            FontId::proportional(9.0),
            color,
        );
        ui.painter().text(
            egui::pos2(x + 12.0, rect.center().y),
            Align2::LEFT_CENTER,
            &entry.name,
            FontId::proportional(11.0),
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
