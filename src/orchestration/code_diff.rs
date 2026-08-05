//! Modelo, parser y carga asíncrona de diffs unificados para el visor de
//! código. El parser es puro y determinístico (testeable sin git); la carga
//! corre en un worker para no bloquear el hilo de UI en repos grandes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender};

use uuid::Uuid;

/// Tipo de línea dentro de un diff unificado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
    HunkHeader,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// Número de línea en el archivo viejo (solo Context/Removed).
    pub old_ln: Option<usize>,
    /// Número de línea en el archivo nuevo (solo Context/Added).
    pub new_ln: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct FileDiff {
    /// Ruta nueva (`b/...`), sin el prefijo.
    pub path: String,
    pub old_path: Option<String>,
    pub is_new: bool,
    pub is_deleted: bool,
    pub is_binary: bool,
    pub additions: usize,
    pub deletions: usize,
    pub lines: Vec<DiffLine>,
}

/// Resultado completo de revisar un repo.
#[derive(Debug, Clone, Default)]
pub struct RepoDiff {
    pub repo_root: PathBuf,
    pub branch: String,
    pub files: Vec<FileDiff>,
    /// True si hay cambios sin trackear que no se pudieron incluir.
    pub has_untracked: bool,
}

impl RepoDiff {
    pub fn total_additions(&self) -> usize {
        self.files.iter().map(|file| file.additions).sum()
    }

    pub fn total_deletions(&self) -> usize {
        self.files.iter().map(|file| file.deletions).sum()
    }
}

/// Parsea un diff unificado completo en una lista de `FileDiff`.
pub fn parse_unified_diff(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut current: Option<FileDiff> = None;
    let mut old_ln: usize = 0;
    let mut new_ln: usize = 0;

    for raw in text.lines() {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            if let Some(finished) = current.take() {
                files.push(finished);
            }
            let file = FileDiff {
                path: parse_diff_git_path(rest),
                ..FileDiff::default()
            };
            current = Some(file);
            old_ln = 0;
            new_ln = 0;
            continue;
        }

        let Some(file) = current.as_mut() else {
            continue;
        };

        if raw.starts_with("new file mode") {
            file.is_new = true;
            continue;
        }
        if raw.starts_with("deleted file mode") {
            file.is_deleted = true;
            continue;
        }
        if raw.starts_with("Binary files") || raw.starts_with("GIT binary patch") {
            file.is_binary = true;
            continue;
        }
        if let Some(rest) = raw.strip_prefix("--- ") {
            let path = strip_ab_prefix(rest);
            if path != "/dev/null" {
                file.old_path = Some(path);
            }
            continue;
        }
        if let Some(rest) = raw.strip_prefix("+++ ") {
            let path = strip_ab_prefix(rest);
            if path != "/dev/null" {
                file.path = path;
            }
            continue;
        }
        if let Some(rest) = raw.strip_prefix("@@ ") {
            if let Some((old_start, new_start)) = parse_hunk_header(rest) {
                old_ln = old_start;
                new_ln = new_start;
            }
            file.lines.push(DiffLine {
                kind: DiffLineKind::HunkHeader,
                old_ln: None,
                new_ln: None,
                text: raw.to_owned(),
            });
            continue;
        }

        if let Some(text) = raw.strip_prefix('+') {
            file.additions += 1;
            file.lines.push(DiffLine {
                kind: DiffLineKind::Added,
                old_ln: None,
                new_ln: Some(new_ln),
                text: text.to_owned(),
            });
            new_ln += 1;
        } else if let Some(text) = raw.strip_prefix('-') {
            file.deletions += 1;
            file.lines.push(DiffLine {
                kind: DiffLineKind::Removed,
                old_ln: Some(old_ln),
                new_ln: None,
                text: text.to_owned(),
            });
            old_ln += 1;
        } else if let Some(text) = raw.strip_prefix(' ') {
            file.lines.push(DiffLine {
                kind: DiffLineKind::Context,
                old_ln: Some(old_ln),
                new_ln: Some(new_ln),
                text: text.to_owned(),
            });
            old_ln += 1;
            new_ln += 1;
        } else if raw == "\\ No newline at end of file" {
            // Marcador: no aporta contenido ni números de línea.
            continue;
        }
    }

    if let Some(finished) = current.take() {
        files.push(finished);
    }
    files
}

/// Extrae la ruta nueva de `diff --git a/<path> b/<path>`. Maneja rutas con
/// espacios tomando el segmento tras el último " b/".
fn parse_diff_git_path(rest: &str) -> String {
    if let Some(index) = rest.rfind(" b/") {
        return rest[index + 3..].to_owned();
    }
    rest.to_owned()
}

fn strip_ab_prefix(path: &str) -> String {
    let path = path.trim();
    if let Some(stripped) = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")) {
        stripped.to_owned()
    } else {
        path.to_owned()
    }
}

/// `@@ -old_start,old_count +new_start,new_count @@ ...` → (old_start, new_start).
fn parse_hunk_header(rest: &str) -> Option<(usize, usize)> {
    let end = rest.find(" @@")?;
    let nums = &rest[..end];
    let mut parts = nums.split_whitespace();
    let old = parts.next()?.trim_start_matches('-');
    let new = parts.next()?.trim_start_matches('+');
    let old_start = old.split(',').next()?.parse().ok()?;
    let new_start = new.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

/// Carga el diff de un repo de forma síncrona (la llama el worker).
pub fn load_repo_diff(repo_root: &Path) -> Option<RepoDiff> {
    let root = git_toplevel(repo_root)?;
    let branch = git_string(&root, &["branch", "--show-current"])
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "detached".to_owned());

    let mut diff_text = git_string(&root, &["diff", "HEAD"]).unwrap_or_default();
    let untracked = list_untracked(&root);
    let has_untracked = !untracked.is_empty();
    for path in &untracked {
        if let Some(synthetic) = synthetic_new_file_diff(&root, path) {
            // git_string recorta el newline final: sin separador la última
            // línea del diff real se fusiona con el `diff --git` sintético.
            if !diff_text.is_empty() && !diff_text.ends_with('\n') {
                diff_text.push('\n');
            }
            diff_text.push_str(&synthetic);
        }
    }

    let files = parse_unified_diff(&diff_text);
    Some(RepoDiff {
        repo_root: root,
        branch,
        files,
        has_untracked,
    })
}

/// Genera un diff "todo agregado" para un archivo nuevo sin trackear. Solo
/// incluye archivos de texto chicos (los binarios/grandes se listan pero no
/// se expanden).
fn synthetic_new_file_diff(root: &Path, rel: &Path) -> Option<String> {
    const MAX_BYTES: u64 = 256 * 1024;
    let full = root.join(rel);
    let meta = std::fs::metadata(&full).ok()?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return None;
    }
    let content = std::fs::read(&full).ok()?;
    if content.contains(&0) {
        return None; // binario
    }
    let text = String::from_utf8_lossy(&content);
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    let lines: Vec<&str> = text.lines().collect();
    let count = lines.len();
    let mut out = String::new();
    out.push_str(&format!("diff --git a/{rel_str} b/{rel_str}\n"));
    out.push_str("new file mode 100644\n");
    out.push_str("--- /dev/null\n");
    out.push_str(&format!("+++ b/{rel_str}\n"));
    out.push_str(&format!("@@ -0,0 +1,{count} @@\n"));
    for line in lines {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    Some(out)
}

fn list_untracked(root: &Path) -> Vec<PathBuf> {
    // --untracked-files=all lista cada archivo nuevo, incluso dentro de
    // directorios nuevos (sin el flag, git solo muestra "dir/").
    let status = match git_string(root, &["status", "--porcelain", "--untracked-files=all"]) {
        Some(status) => status,
        None => return Vec::new(),
    };
    status
        .lines()
        .filter_map(|line| {
            let code = line.get(0..2)?;
            if code != "??" {
                return None;
            }
            let path = line.get(3..)?.trim();
            if path.is_empty() || path.ends_with('/') {
                return None;
            }
            Some(PathBuf::from(path))
        })
        .collect()
}

fn git_toplevel(path: &Path) -> Option<PathBuf> {
    git_string(path, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// Un worktree listado por `git worktree list --porcelain`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
    pub is_main: bool,
}

/// Lista los worktrees del repo (parseo de `git worktree list --porcelain`).
pub fn list_git_worktrees(repo_root: &Path) -> Vec<WorktreeInfo> {
    let root = match git_toplevel(repo_root) {
        Some(root) => root,
        None => return Vec::new(),
    };
    let raw = match git_string(&root, &["worktree", "list", "--porcelain"]) {
        Some(raw) => raw,
        None => return Vec::new(),
    };
    let mut worktrees: Vec<WorktreeInfo> = Vec::new();
    let mut current: Option<WorktreeInfo> = None;
    for line in raw.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            // Cierra el worktree anterior si hay uno abierto.
            if let Some(finished) = current.take() {
                worktrees.push(finished);
            }
            current = Some(WorktreeInfo {
                path: PathBuf::from(path.trim()),
                branch: String::new(),
                is_main: false,
            });
        } else if let Some(worktree) = current.as_mut() {
            if let Some(branch) = line.strip_prefix("branch ") {
                worktree.branch = branch.trim().trim_start_matches("refs/heads/").to_owned();
            } else if line.trim() == "detached" {
                worktree.branch = "detached".to_owned();
            }
        }
    }
    if let Some(finished) = current.take() {
        worktrees.push(finished);
    }
    // El primer worktree listado es el principal.
    if let Some(first) = worktrees.first_mut() {
        first.is_main = true;
    }
    worktrees
}

/// Remueve un worktree (solo los vinculados bajo `.terminalcanvas/worktrees`,
/// nunca el principal). Usa `--force` porque suelen quedar cambios sin
/// commitear del agente.
pub fn remove_git_worktree(repo_root: &Path, worktree_path: &Path) -> anyhow::Result<()> {
    let root =
        git_toplevel(repo_root).ok_or_else(|| anyhow::anyhow!("No es un repositorio git"))?;
    let is_managed = worktree_path.components().any(|component| {
        matches!(component, std::path::Component::Normal(name) if name == ".terminalcanvas")
    });
    if !is_managed {
        anyhow::bail!("Solo se limpian worktrees bajo .terminalcanvas/worktrees");
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["worktree", "remove", "--force"])
        .arg(worktree_path)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git worktree remove failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn git_string(path: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim_end().to_owned())
}

#[derive(Debug)]
pub struct DiffLoadRequest {
    pub key: Uuid,
    pub repo_root: PathBuf,
}

#[derive(Debug)]
pub struct DiffLoadResult {
    pub key: Uuid,
    pub diff: Option<RepoDiff>,
}

/// Worker que carga diffs en un hilo dedicado (igual patrón que
/// `GitInspector`): la UI encola y pollea, nunca bloquea.
#[derive(Debug, Default)]
pub struct DiffLoader {
    worker: Option<DiffLoaderWorker>,
}

#[derive(Debug)]
struct DiffLoaderWorker {
    request_tx: Sender<DiffLoadRequest>,
    result_rx: Receiver<DiffLoadResult>,
}

impl DiffLoader {
    pub fn request(&mut self, key: Uuid, repo_root: PathBuf) {
        if self.worker.is_none() {
            self.worker = spawn_diff_worker();
        }
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        if worker
            .request_tx
            .send(DiffLoadRequest { key, repo_root })
            .is_err()
        {
            self.worker = None;
        }
    }

    pub fn poll(&mut self) -> Vec<DiffLoadResult> {
        let Some(worker) = self.worker.as_ref() else {
            return Vec::new();
        };
        let mut results = Vec::new();
        while let Ok(result) = worker.result_rx.try_recv() {
            results.push(result);
        }
        results
    }
}

fn spawn_diff_worker() -> Option<DiffLoaderWorker> {
    let (request_tx, request_rx) = std::sync::mpsc::channel::<DiffLoadRequest>();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("diff-loader".to_owned())
        .spawn(move || {
            while let Ok(request) = request_rx.recv() {
                let diff = load_repo_diff(&request.repo_root);
                let result = DiffLoadResult {
                    key: request.key,
                    diff,
                };
                if result_tx.send(result).is_err() {
                    break;
                }
            }
        })
        .ok()?;
    Some(DiffLoaderWorker {
        request_tx,
        result_rx,
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_hunk_header, parse_unified_diff, DiffLineKind};

    const SAMPLE: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,5 +1,6 @@ fn main
 use std::io;
-old_line();
+new_line();
+another_new();
 
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
 }
";

    #[test]
    fn parses_single_file_with_mixed_lines() {
        let files = parse_unified_diff(SAMPLE);
        assert_eq!(files.len(), 1);
        let file = &files[0];
        assert_eq!(file.path, "src/main.rs");
        assert_eq!(file.old_path.as_deref(), Some("src/main.rs"));
        assert!(!file.is_new);
        assert!(!file.is_deleted);
        assert_eq!(file.additions, 3);
        assert_eq!(file.deletions, 2);
    }

    #[test]
    fn assigns_line_numbers_per_hunk() {
        let files = parse_unified_diff(SAMPLE);
        let file = &files[0];
        let added: Vec<_> = file
            .lines
            .iter()
            .filter(|line| line.kind == DiffLineKind::Added)
            .collect();
        assert_eq!(added.len(), 3);
        assert_eq!(added[0].new_ln, Some(2));
        assert_eq!(added[1].new_ln, Some(3));
        let removed: Vec<_> = file
            .lines
            .iter()
            .filter(|line| line.kind == DiffLineKind::Removed)
            .collect();
        assert_eq!(removed.len(), 2);
        assert_eq!(removed[0].old_ln, Some(2));
    }

    #[test]
    fn parses_new_file_diff() {
        let text = "\
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
";
        let files = parse_unified_diff(text);
        assert_eq!(files.len(), 1);
        assert!(files[0].is_new);
        assert_eq!(files[0].path, "new.txt");
        assert_eq!(files[0].additions, 2);
        assert_eq!(files[0].deletions, 0);
    }

    #[test]
    fn parses_deleted_and_binary_files() {
        let text = "\
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 4444444..0000000
--- a/gone.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-bye
diff --git a/img.png b/img.png
index 5555555..6666666 100644
Binary files a/img.png and b/img.png differ
";
        let files = parse_unified_diff(text);
        assert_eq!(files.len(), 2);
        assert!(files[0].is_deleted);
        assert_eq!(files[0].deletions, 1);
        assert!(files[1].is_binary);
    }

    #[test]
    fn parses_multiple_files() {
        let text = "\
diff --git a/a.rs b/a.rs
index 1..2 100644
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-x
+y
diff --git a/b.rs b/b.rs
index 3..4 100644
--- a/b.rs
+++ b/b.rs
@@ -1 +1,2 @@
 keep
+added
";
        let files = parse_unified_diff(text);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "a.rs");
        assert_eq!(files[1].path, "b.rs");
        assert_eq!(files[1].additions, 1);
    }

    #[test]
    fn hunk_header_parses_start_lines() {
        assert_eq!(parse_hunk_header("-10,3 +20,4 @@ fn foo"), Some((10, 20)));
        assert_eq!(parse_hunk_header("-1 +1 @@ top"), Some((1, 1)));
        assert_eq!(parse_hunk_header("garbage"), None);
    }

    #[test]
    fn handles_paths_with_spaces() {
        let text = "\
diff --git a/my file.txt b/my file.txt
index 1..2 100644
--- a/my file.txt
+++ b/my file.txt
@@ -1 +1 @@
-a
+b
";
        let files = parse_unified_diff(text);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "my file.txt");
    }

    #[test]
    fn load_repo_diff_reads_a_real_git_repo() {
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!("diff-load-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");

        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("tracked.txt"), "line1\nline2\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "initial"]);

        // Modificar el archivo trackeado y crear nuevos sin trackear (uno
        // suelto y otro dentro de un directorio nuevo).
        std::fs::write(dir.join("tracked.txt"), "line1\nchanged\n").unwrap();
        std::fs::write(dir.join("new_file.txt"), "brand new\n").unwrap();
        std::fs::create_dir_all(dir.join("newdir")).unwrap();
        std::fs::write(dir.join("newdir/nested.txt"), "nested\n").unwrap();

        let diff = super::load_repo_diff(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        let diff = diff.expect("repo diff should load");
        assert!(diff.branch == "master" || diff.branch == "main");
        let paths: Vec<&str> = diff.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"tracked.txt"), "tracked change present");
        assert!(paths.contains(&"new_file.txt"), "untracked file present");
        assert!(
            paths.contains(&"newdir/nested.txt"),
            "untracked file inside a new directory present"
        );
        let new_file = diff
            .files
            .iter()
            .find(|f| f.path == "new_file.txt")
            .unwrap();
        assert!(new_file.is_new);
        assert_eq!(new_file.additions, 1);
    }

    #[test]
    fn list_and_remove_managed_worktrees() {
        use std::process::Command;
        let dir =
            std::env::temp_dir().join(format!("worktree-lifecycle-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");

        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("file.txt"), "hello\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "initial"]);

        // Worktree gestionado bajo .terminalcanvas/worktrees.
        let wt_path = dir.join(".terminalcanvas/worktrees/claude/test-agent");
        git(&[
            "worktree",
            "add",
            "-b",
            "workspace/claude/test-agent",
            wt_path.to_str().unwrap(),
        ]);
        // git reporta paths canonicalizados (macOS: /var → /private/var).
        let wt_path = std::fs::canonicalize(&wt_path).expect("canonicalize worktree path");

        let worktrees = super::list_git_worktrees(&dir);
        assert!(worktrees.len() >= 2, "expected main + linked worktrees");
        assert!(worktrees[0].is_main, "first worktree is main");
        let managed = worktrees
            .iter()
            .find(|wt| wt.path == wt_path)
            .expect("managed worktree listed");
        assert!(!managed.is_main);

        // Remover el worktree gestionado.
        super::remove_git_worktree(&dir, &wt_path).expect("remove managed worktree");
        let after = super::list_git_worktrees(&dir);
        assert!(
            after.iter().all(|wt| wt.path != wt_path),
            "removed worktree no longer listed"
        );

        // Remover un worktree no gestionado (el principal) debe fallar.
        let main_path = after[0].path.clone();
        assert!(super::remove_git_worktree(&dir, &main_path).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
