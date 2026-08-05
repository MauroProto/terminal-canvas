use std::sync::atomic::{AtomicBool, Ordering};

use egui::FontFamily;

/// Familia egui que mapea a la variante bold de la fuente monoespaciada del
/// terminal. Si no se pudo cargar ninguna variante bold en `setup_fonts`,
/// `bold_font_available()` es false y el renderer cae al brillo simulado.
pub const TERMINAL_BOLD_FONT: &str = "terminal_bold";

static BOLD_FONT_AVAILABLE: AtomicBool = AtomicBool::new(false);

pub fn bold_font_available() -> bool {
    BOLD_FONT_AVAILABLE.load(Ordering::Relaxed)
}

fn register_bold_family(fonts: &mut egui::FontDefinitions, font_name: &str) {
    fonts
        .families
        .entry(FontFamily::Name(TERMINAL_BOLD_FONT.into()))
        .or_default()
        .insert(0, font_name.to_owned());
    BOLD_FONT_AVAILABLE.store(true, Ordering::Relaxed);
}

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
        // Consolas Bold.
        if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\consolab.ttf") {
            fonts.font_data.insert(
                "consolas_bold".into(),
                egui::FontData::from_owned(data).into(),
            );
            register_bold_family(&mut fonts, "consolas_bold");
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
        // Menlo Bold: índice 1 dentro del .ttc.
        if let Ok(data) = std::fs::read("/System/Library/Fonts/Menlo.ttc") {
            let mut bold_data = egui::FontData::from_owned(data);
            bold_data.index = 1;
            fonts
                .font_data
                .insert("menlo_bold".into(), bold_data.into());
            register_bold_family(&mut fonts, "menlo_bold");
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
        let bold_paths = [
            "/usr/share/fonts/truetype/noto/NotoSansMono-Bold.ttf",
            "/usr/share/fonts/noto/NotoSansMono-Bold.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
            "/usr/share/fonts/dejavu/DejaVuSansMono-Bold.ttf",
        ];
        for path in bold_paths {
            if let Ok(data) = std::fs::read(path) {
                fonts.font_data.insert(
                    "system_mono_bold".into(),
                    egui::FontData::from_owned(data).into(),
                );
                register_bold_family(&mut fonts, "system_mono_bold");
                break;
            }
        }
    }

    cc.egui_ctx.set_fonts(fonts);
}
