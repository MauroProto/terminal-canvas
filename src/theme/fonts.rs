use egui::FontFamily;

pub fn setup_fonts(cc: &eframe::CreationContext<'_>) {
    let mut fonts = egui::FontDefinitions::default();

    #[cfg(target_os = "windows")]
    {
        if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\seguisym.ttf") {
            fonts.font_data.insert(
                "segoe_symbol".into(),
                egui::FontData::from_owned(data).into(),
            );
            fonts
                .families
                .get_mut(&FontFamily::Monospace)
                .unwrap()
                .push("segoe_symbol".into());
            fonts
                .families
                .get_mut(&FontFamily::Proportional)
                .unwrap()
                .push("segoe_symbol".into());
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(data) = std::fs::read("/System/Library/Fonts/Menlo.ttc") {
            fonts
                .font_data
                .insert("menlo".into(), egui::FontData::from_owned(data).into());
            fonts
                .families
                .get_mut(&FontFamily::Monospace)
                .unwrap()
                .insert(0, "menlo".into());
        }
        if let Ok(data) = std::fs::read("/System/Library/Fonts/Apple Symbols.ttf") {
            fonts.font_data.insert(
                "apple_symbols".into(),
                egui::FontData::from_owned(data).into(),
            );
            fonts
                .families
                .get_mut(&FontFamily::Monospace)
                .unwrap()
                .push("apple_symbols".into());
            fonts
                .families
                .get_mut(&FontFamily::Proportional)
                .unwrap()
                .push("apple_symbols".into());
        }
    }

    #[cfg(target_os = "linux")]
    {
        let paths = [
            "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
        ];
        for path in paths {
            if let Ok(data) = std::fs::read(path) {
                fonts.font_data.insert(
                    "system_mono".into(),
                    egui::FontData::from_owned(data).into(),
                );
                fonts
                    .families
                    .get_mut(&FontFamily::Monospace)
                    .unwrap()
                    .push("system_mono".into());
                break;
            }
        }
    }

    cc.egui_ctx.set_fonts(fonts);
}
