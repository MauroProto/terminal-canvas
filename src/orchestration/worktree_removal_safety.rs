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

fn resolve_link(base: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value.trim());
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn read_gitdir_link(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    let value = text.trim().strip_prefix("gitdir:")?.trim();
    if value.is_empty() || text.lines().count() != 1 {
        return None;
    }
    Some(resolve_link(path.parent()?, value))
}

fn common_git_dir(repo_root: &Path) -> Option<PathBuf> {
    let dot_git = repo_root.join(".git");
    if dot_git.is_dir() {
        return std::fs::canonicalize(dot_git).ok();
    }
    let gitdir = std::fs::canonicalize(read_gitdir_link(&dot_git)?).ok()?;
    let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    std::fs::canonicalize(resolve_link(&gitdir, &common)).ok()
}

/// Comprueba el enlace en ambas direcciones y que el gitdir pertenece al
/// `common dir` del repositorio. Un archivo `.git` inventado ya no alcanza.
fn is_linked_worktree(path: &Path, repo_root: &Path) -> bool {
    let dot_git = path.join(".git");
    if !dot_git.is_file() {
        return false;
    }
    let Some(common) = common_git_dir(repo_root) else {
        return false;
    };
    let Some(target) =
        read_gitdir_link(&dot_git).and_then(|target| std::fs::canonicalize(target).ok())
    else {
        return false;
    };
    let Some(worktrees_dir) = target.parent() else {
        return false;
    };
    if worktrees_dir.file_name().and_then(|name| name.to_str()) != Some("worktrees")
        || worktrees_dir.parent() != Some(common.as_path())
    {
        return false;
    }
    let target_common = std::fs::read_to_string(target.join("commondir"))
        .ok()
        .map(|value| resolve_link(&target, &value))
        .and_then(|path| std::fs::canonicalize(path).ok());
    if target_common.as_deref() != Some(common.as_path()) {
        return false;
    }
    let backlink = std::fs::read_to_string(target.join("gitdir"))
        .ok()
        .map(|value| resolve_link(&target, &value))
        .and_then(|path| std::fs::canonicalize(path).ok());
    let canonical_dot_git = std::fs::canonicalize(dot_git).ok();
    backlink.is_some() && backlink == canonical_dot_git
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
    let canonical_path = std::fs::canonicalize(path)
        .map_err(|_| RemovalGuard("el path a borrar no se puede canonicalizar".into()))?;
    let canonical_repo = std::fs::canonicalize(repo_root)
        .map_err(|_| RemovalGuard("el repositorio no se puede canonicalizar".into()))?;
    // `/` y el repo mismo nunca se borran.
    if canonical_path == Path::new("/") {
        return Err(RemovalGuard(
            "no se puede borrar la raíz del sistema".into(),
        ));
    }
    if canonical_path == canonical_repo {
        return Err(RemovalGuard("el path es el repositorio".into()));
    }
    // Un ancestro del repo se llevaría el repo puesto.
    if canonical_repo.starts_with(&canonical_path) {
        return Err(RemovalGuard("el path es ancestro del repositorio".into()));
    }
    // El home del usuario nunca.
    if let Some(home) = home_dir() {
        if std::fs::canonicalize(home).ok().as_deref() == Some(canonical_path.as_path()) {
            return Err(RemovalGuard("el path es el home del usuario".into()));
        }
    }
    // Si contiene otro worktree registrado, borrarlo lo rompería.
    for worktree in registered_worktrees {
        let Some(worktree) = std::fs::canonicalize(worktree).ok() else {
            continue;
        };
        if worktree != canonical_path && worktree.starts_with(&canonical_path) {
            return Err(RemovalGuard(
                "el path contiene otro worktree registrado".into(),
            ));
        }
    }
    // La forma del path no es autoridad: sin el `.git` link no es un worktree
    // real y no lo tocamos.
    if !is_linked_worktree(&canonical_path, &canonical_repo) {
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

    fn repo(tag: &str) -> PathBuf {
        let repo = temp(tag);
        fs::create_dir_all(repo.join(".git/worktrees")).unwrap();
        repo
    }

    fn linked_worktree(tag: &str, repo: &Path) -> PathBuf {
        let dir = temp(tag);
        fs::create_dir_all(&dir).unwrap();
        let metadata = repo
            .join(".git/worktrees")
            .join(uuid::Uuid::new_v4().simple().to_string());
        fs::create_dir_all(&metadata).unwrap();
        fs::write(metadata.join("commondir"), "../..\n").unwrap();
        fs::write(
            metadata.join("gitdir"),
            format!("{}\n", dir.join(".git").display()),
        )
        .unwrap();
        fs::write(
            dir.join(".git"),
            format!("gitdir: {}\n", metadata.display()),
        )
        .unwrap();
        dir
    }

    #[test]
    fn rejects_the_repo_root_itself() {
        let repo = repo("repo");
        let wt = linked_worktree("wt-repo", &repo);
        let err = check_recursive_delete(&repo, &repo, std::slice::from_ref(&wt));
        let _ = fs::remove_dir_all(&wt);
        let _ = fs::remove_dir_all(&repo);
        assert!(err.is_err(), "repo_root debe rechazarse");
    }

    #[test]
    fn rejects_an_ancestor_of_the_repo() {
        let parent = temp("ancestor");
        let repo = parent.join("sub");
        fs::create_dir_all(repo.join(".git/worktrees")).unwrap();
        let parent = repo.parent().unwrap().to_path_buf();
        let wt = linked_worktree("wt-anc", &repo);
        let err = check_recursive_delete(&parent, &repo, std::slice::from_ref(&wt));
        let _ = fs::remove_dir_all(&wt);
        let _ = fs::remove_dir_all(&parent);
        assert!(err.is_err(), "ancestro del repo debe rechazarse");
    }

    #[test]
    fn rejects_the_system_root() {
        let repo = repo("repo-root");
        let err = check_recursive_delete(Path::new("/"), &repo, &[]);
        let _ = fs::remove_dir_all(&repo);
        assert!(err.is_err(), "/ debe rechazarse");
    }

    #[test]
    fn rejects_a_path_containing_another_registered_worktree() {
        let repo = repo("repo-nested");
        let outer = linked_worktree("outer", &repo);
        let inner = linked_worktree("inner", &repo);
        // Simulamos que `inner` está dentro de `outer`.
        let nested_inner = outer.join("nested");
        fs::create_dir_all(&nested_inner).unwrap();
        fs::write(nested_inner.join(".git"), "gitdir: x\n").unwrap();

        let err = check_recursive_delete(&outer, &repo, std::slice::from_ref(&nested_inner));
        let _ = fs::remove_dir_all(&outer);
        let _ = fs::remove_dir_all(&inner);
        let _ = fs::remove_dir_all(&repo);
        assert!(err.is_err(), "contener otro worktree debe rechazarse");
    }

    #[test]
    fn path_shape_is_not_authority_without_git_link() {
        // Un directorio con nombre de worktree pero sin `.git` no se toca.
        let fake = temp("fake-worktree");
        fs::create_dir_all(&fake).unwrap();
        let repo = repo("repo-fake");
        let err = check_recursive_delete(&fake, &repo, &[]);
        let _ = fs::remove_dir_all(&fake);
        let _ = fs::remove_dir_all(&repo);
        assert!(
            err.is_err(),
            "sin .git link la forma del path no es autoridad"
        );
    }

    #[test]
    fn allows_a_real_linked_worktree() {
        let repo = repo("repo-ok");
        let wt = linked_worktree("ok", &repo);
        let result = check_recursive_delete(&wt, &repo, std::slice::from_ref(&wt));
        let _ = fs::remove_dir_all(&wt);
        let _ = fs::remove_dir_all(&repo);
        assert!(result.is_ok(), "worktree real vinculado debe pasar");
    }

    #[test]
    fn rejects_a_git_file_pointing_outside_the_repository_metadata() {
        let repo = repo("repo-forged");
        let fake = temp("forged");
        let foreign = temp("foreign-metadata");
        fs::create_dir_all(&fake).unwrap();
        fs::create_dir_all(&foreign).unwrap();
        fs::write(
            fake.join(".git"),
            format!("gitdir: {}\n", foreign.display()),
        )
        .unwrap();

        let result = check_recursive_delete(&fake, &repo, &[]);
        let _ = fs::remove_dir_all(&fake);
        let _ = fs::remove_dir_all(&foreign);
        let _ = fs::remove_dir_all(&repo);
        assert!(result.is_err());
    }
}
