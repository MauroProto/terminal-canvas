pub mod panel_state;
pub mod persistence;
pub mod workspace;

pub use panel_state::{PanelPlacement, PanelState, SnapSlot};
pub use persistence::{load_state, save_state, AppState, WorkspaceState};
pub use workspace::{TerminalSpawnRequest, Workspace};
