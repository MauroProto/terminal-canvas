#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod canvas;
mod collab;
mod command_palette;
mod orchestration;
mod panel;
mod runtime;
mod shortcuts;
mod sidebar;
mod state;
mod terminal;
mod theme;
mod update;
mod utils;

use std::sync::Arc;
use std::{backtrace::Backtrace, fmt::Write as _, fs, io::Write as _, path::Path, path::PathBuf};

use anyhow::Result;

fn main() -> Result<()> {
    env_logger::init();
    install_panic_logging();
    log_terminal_backends();
    let pending_join_invite = collab::invite_code_from_launch_sources(
        std::env::args(),
        std::env::var("TERMINAL_CANVAS_JOIN_INVITE").ok(),
    );

    let icon = {
        let icon_data = include_bytes!("../assets/icon.png");
        let img = image::load_from_memory(icon_data)
            .expect("Failed to load icon")
            .to_rgba8();
        let (w, h) = img.dimensions();
        egui::IconData {
            rgba: img.into_raw(),
            width: w,
            height: h,
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("My Terminal | v{}", env!("CARGO_PKG_VERSION")))
            .with_inner_size([1024.0, 640.0])
            .with_min_inner_size([640.0, 400.0])
            .with_icon(Arc::new(icon)),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "My Terminal",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::TerminalApp::new(
                cc,
                pending_join_invite.clone(),
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}

fn log_terminal_backends() {
    let current = terminal::backend::TerminalBackendStatus::alacritty();
    let ghostty = terminal::backend::TerminalBackendStatus::ghostty();
    log::info!(
        "terminal backend: {} ({})",
        current.kind.as_str(),
        current.reason
    );
    log::info!(
        "terminal backend candidate: {} available={} actor_required={} ({})",
        ghostty.kind.as_str(),
        ghostty.available,
        ghostty.requires_single_thread_actor,
        ghostty.reason
    );
}

fn install_panic_logging() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(home) = utils::platform::home_dir() {
            let path = panic_log_path(&home);
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
                let payload = panic_payload(info);
                let location = info.location().map(|location| {
                    format!(
                        "{}:{}:{}",
                        location.file(),
                        location.line(),
                        location.column()
                    )
                });
                let report = format_panic_report(
                    &payload,
                    location.as_deref(),
                    &Backtrace::force_capture().to_string(),
                );
                let _ = file.write_all(report.as_bytes());
            }
        }
        default_hook(info);
    }));
}

fn panic_payload(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(payload) = info.payload().downcast_ref::<&str>() {
        (*payload).to_owned()
    } else if let Some(payload) = info.payload().downcast_ref::<String>() {
        payload.clone()
    } else {
        "panic without string payload".to_owned()
    }
}

fn panic_log_path(home: &Path) -> PathBuf {
    home.join(".mi-terminal").join("logs").join("panic.log")
}

fn format_panic_report(message: &str, location: Option<&str>, backtrace: &str) -> String {
    let mut report = String::new();
    let _ = writeln!(report, "=== panic {} ===", chrono::Utc::now().to_rfc3339());
    let _ = writeln!(report, "message: {message}");
    if let Some(location) = location {
        let _ = writeln!(report, "location: {location}");
    }
    let _ = writeln!(report, "backtrace:");
    let _ = writeln!(report, "{backtrace}");
    let _ = writeln!(report);
    report
}

#[cfg(test)]
mod tests {
    use super::{format_panic_report, panic_log_path};
    use std::path::Path;

    #[test]
    fn panic_log_path_targets_hidden_log_file() {
        let path = panic_log_path(Path::new("/tmp/home"));
        assert_eq!(path, Path::new("/tmp/home/.mi-terminal/logs/panic.log"));
    }

    #[test]
    fn panic_report_includes_message_location_and_backtrace() {
        let report = format_panic_report("boom", Some("src/main.rs:12:3"), "stack");

        assert!(report.contains("message: boom"));
        assert!(report.contains("location: src/main.rs:12:3"));
        assert!(report.contains("backtrace:"));
        assert!(report.contains("stack"));
    }
}
