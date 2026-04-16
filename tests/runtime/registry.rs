mod terminal {
    #![allow(dead_code)]

    #[path = "../../../src/terminal/colors.rs"]
    pub mod colors;
    #[path = "../../../src/terminal/input.rs"]
    pub mod input;
    #[path = "../../../src/terminal/pty.rs"]
    pub mod pty;
}

mod utils {
    #![allow(dead_code)]

    #[path = "../../../src/utils/platform.rs"]
    pub mod platform;
}

#[allow(dead_code)]
#[path = "../../src/runtime/mod.rs"]
mod runtime;

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
