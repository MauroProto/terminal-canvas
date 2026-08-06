//! Salvaguardas de borrado recursivo de worktrees (P1.9).
//!
//! Antes de CUALQUIER `remove_dir_all` sobre un worktree se pasa por
//! [`check_recursive_delete`], que rechaza el borrado si el path es el repo,
//! un ancestro del repo, `/`, un home, contiene otro worktree registrado, o si
//! la "forma" del path no alcanza para probar que es un worktree real (falta el
//! archivo `.git` que vincula al repo). La forma del path nunca es autoridad.

use std::path::{Path, PathBuf};

/// Error de salvaguarda: el borrado se rechaza con un motivo legible.
#[derive(Debug, PartialEq, Eq)]
pub struct RemovalGuard(pub String);

/// ¿El path tiene el archivo `.git` (gitdir link) que prueba que es un
/// worktree real vinculado al repo? La forma del nombre no es autoridad.
fn is_linked_worktree(path: &Path) -> bool {
    path.join(".git").is_file()
}

fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|base| base.home_dir().to_path_buf())
}

/// Valida que sea seguro borrar `path` recursivamente. Devuelve `Err` con el
/// motivo si alguna salvaguarda salta.
pub fn check_recursive_delete(
    path: &Path,
    repo_root: &Path,
    registered_worktrees: &[PathBuf],
) -> Result<(), RemovalGuard> {
    // `/` y el repo mismo nunca se borran.
    if path == Path::new("/") {
        return Err(RemovalGuard(
            "no se puede borrar la raíz del sistema".into(),
        ));
    }
    if path == repo_root {
        return Err(RemovalGuard("el path es el repositorio".into()));
    }
    // Un ancestro del repo se llevaría el repo puesto.
    if repo_root.starts_with(path) {
        return Err(RemovalGuard("el path es ancestro del repositorio".into()));
    }
    // El home del usuario nunca.
    if let Some(home) = home_dir() {
        if path == home {
            return Err(RemovalGuard("el path es el home del usuario".into()));
        }
    }
    // Si contiene otro worktree registrado, borrarlo lo rompería.
    for worktree in registered_worktrees {
        if worktree != path && worktree.starts_with(path) {
            return Err(RemovalGuard(
                "el path contiene otro worktree registrado".into(),
            ));
        }
    }
    // La forma del path no es autoridad: sin el `.git` link no es un worktree
    // real y no lo tocamos.
    if !is_linked_worktree(path) {
        return Err(RemovalGuard(
            "el path no tiene el .git que prueba el vínculo al repo".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_recursive_delete;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wt-safety-{tag}-{}", uuid::Uuid::new_v4()))
    }

    fn linked_worktree(tag: &str) -> PathBuf {
        let dir = temp(tag);
        fs::create_dir_all(&dir).unwrap();
        // El `.git` de un worktree es un *archivo* que apunta al gitdir real.
        fs::write(dir.join(".git"), "gitdir: /fake/repo/.git/worktrees/x\n").unwrap();
        dir
    }

    #[test]
    fn rejects_the_repo_root_itself() {
        let repo = temp("repo");
        let wt = linked_worktree("wt-repo");
        let err = check_recursive_delete(&repo, &repo, std::slice::from_ref(&wt));
        let _ = fs::remove_dir_all(&wt);
        assert!(err.is_err(), "repo_root debe rechazarse");
    }

    #[test]
    fn rejects_an_ancestor_of_the_repo() {
        let repo = temp("repo").join("sub");
        let parent = repo.parent().unwrap().to_path_buf();
        let wt = linked_worktree("wt-anc");
        let err = check_recursive_delete(&parent, &repo, std::slice::from_ref(&wt));
        let _ = fs::remove_dir_all(&wt);
        assert!(err.is_err(), "ancestro del repo debe rechazarse");
    }

    #[test]
    fn rejects_the_system_root() {
        let repo = temp("repo");
        let err = check_recursive_delete(Path::new("/"), &repo, &[]);
        assert!(err.is_err(), "/ debe rechazarse");
    }

    #[test]
    fn rejects_a_path_containing_another_registered_worktree() {
        let repo = temp("repo");
        let outer = linked_worktree("outer");
        let inner = linked_worktree("inner");
        // Simulamos que `inner` está dentro de `outer`.
        let nested_inner = outer.join("nested");
        fs::create_dir_all(&nested_inner).unwrap();
        fs::write(nested_inner.join(".git"), "gitdir: x\n").unwrap();

        let err = check_recursive_delete(&outer, &repo, std::slice::from_ref(&nested_inner));
        let _ = fs::remove_dir_all(&outer);
        let _ = fs::remove_dir_all(&inner);
        assert!(err.is_err(), "contener otro worktree debe rechazarse");
    }

    #[test]
    fn path_shape_is_not_authority_without_git_link() {
        // Un directorio con nombre de worktree pero sin `.git` no se toca.
        let fake = temp("fake-worktree");
        fs::create_dir_all(&fake).unwrap();
        let repo = temp("repo");
        let err = check_recursive_delete(&fake, &repo, &[]);
        let _ = fs::remove_dir_all(&fake);
        assert!(
            err.is_err(),
            "sin .git link la forma del path no es autoridad"
        );
    }

    #[test]
    fn allows_a_real_linked_worktree() {
        let repo = temp("repo");
        let wt = linked_worktree("ok");
        let result = check_recursive_delete(&wt, &repo, std::slice::from_ref(&wt));
        let _ = fs::remove_dir_all(&wt);
        assert!(result.is_ok(), "worktree real vinculado debe pasar");
    }
}
