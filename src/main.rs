#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]

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

use anyhow::Result;

fn main() -> Result<()> {
    env_logger::init();
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
