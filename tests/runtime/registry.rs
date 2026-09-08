use mi_terminal::runtime;

use runtime::RuntimeRegistry;

#[test]
fn registry_groups_sessions_by_workspace() {
    let mut registry = RuntimeRegistry::new();
    let workspace = registry.create_workspace("api", None);
    let session = registry.create_session(workspace);

    let snapshot = registry.snapshot();

    assert_eq!(snapshot.workspaces.len(), 1);
    assert_eq!(snapshot.sessions_by_workspace(&workspace).len(), 1);
    assert_eq!(snapshot.sessions_by_workspace(&workspace)[0].id, session);
}
