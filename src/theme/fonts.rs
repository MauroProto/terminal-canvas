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

/// Complete the bold family after every platform font has been installed.
/// In particular, macOS adds Apple Symbols after registering Menlo Bold.
/// Keep the bold face first while retaining the final monospace fallback order.
fn complete_bold_family_fallbacks(fonts: &mut egui::FontDefinitions) {
    let fallbacks = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    let Some(bold_family) = fonts
        .families
        .get_mut(&FontFamily::Name(TERMINAL_BOLD_FONT.into()))
    else {
        return;
    };

    let mut complete = Vec::with_capacity(bold_family.len() + fallbacks.len());
    for name in bold_family.iter().chain(fallbacks.iter()) {
        if !complete.contains(name) {
            complete.push(name.clone());
        }
    }
    *bold_family = complete;
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

    complete_bold_family_fallbacks(&mut fonts);
    cc.egui_ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use egui::{FontDefinitions, FontFamily, FontId, RawInput};

    use super::{complete_bold_family_fallbacks, TERMINAL_BOLD_FONT};

    fn bold_family() -> FontFamily {
        FontFamily::Name(TERMINAL_BOLD_FONT.into())
    }

    #[test]
    fn bold_family_retains_primary_and_final_monospace_fallback_order() {
        let mut fonts = FontDefinitions::empty();
        // Install the bold face first, then the symbols, just as on macOS.
        fonts
            .families
            .insert(bold_family(), vec!["bold".into(), "bold".into()]);
        fonts.families.insert(
            FontFamily::Monospace,
            vec!["normal".into(), "emoji".into(), "bold".into()],
        );
        fonts
            .families
            .get_mut(&FontFamily::Monospace)
            .unwrap()
            .extend(["symbols".into(), "emoji".into()]);

        complete_bold_family_fallbacks(&mut fonts);

        assert_eq!(
            fonts.families[&bold_family()],
            ["bold", "normal", "emoji", "symbols"]
        );
        assert_eq!(
            fonts.families[&FontFamily::Monospace],
            ["normal", "emoji", "bold", "symbols", "emoji"]
        );
        let first = fonts.clone();
        complete_bold_family_fallbacks(&mut fonts);
        assert_eq!(fonts, first, "completing a family twice must be idempotent");
    }

    #[test]
    fn completing_fallbacks_does_not_create_an_unavailable_bold_family() {
        let mut fonts = FontDefinitions::default();
        let original = fonts.clone();

        complete_bold_family_fallbacks(&mut fonts);

        assert_eq!(fonts, original);
        assert!(!fonts.families.contains_key(&bold_family()));
    }

    #[test]
    fn bold_family_keeps_its_primary_without_a_monospace_family() {
        let mut fonts = FontDefinitions::empty();
        fonts.families.remove(&FontFamily::Monospace);
        fonts
            .families
            .insert(bold_family(), vec!["bold".into(), "bold".into()]);

        complete_bold_family_fallbacks(&mut fonts);

        assert_eq!(fonts.families[&bold_family()], ["bold"]);
        assert!(!fonts.families.contains_key(&FontFamily::Monospace));
    }

    #[test]
    fn bold_family_can_render_an_embedded_emoji_added_after_its_primary() {
        // These are egui's embedded fonts, not files from the operating system.
        // Hack is only a stand-in for the primary bold face; this test covers
        // fallback selection, not the visual weight of a system font.
        let mut fonts = FontDefinitions::default();
        fonts.families.insert(bold_family(), vec!["Hack".into()]);
        fonts.families.insert(
            FontFamily::Monospace,
            vec!["Hack".into(), "NotoEmoji-Regular".into()],
        );
        let font = FontId::new(15.0, bold_family());
        let before = font_context(fonts.clone());
        assert!(before.fonts_mut(|fonts| fonts.has_glyph(&font, 'A')));
        assert!(!before.fonts_mut(|fonts| fonts.has_glyph(&font, '\u{1f600}')));
        assert!(
            before.fonts_mut(|fonts| { fonts.has_glyph(&FontId::monospace(15.0), '\u{1f600}') })
        );

        complete_bold_family_fallbacks(&mut fonts);

        let after = font_context(fonts);
        assert!(after.fonts_mut(|fonts| fonts.has_glyph(&font, 'A')));
        assert!(after.fonts_mut(|fonts| fonts.has_glyph(&font, '\u{1f600}')));
        let galley = after.fonts_mut(|fonts| {
            fonts.layout_no_wrap("A\u{1f600}".into(), font.clone(), egui::Color32::WHITE)
        });
        assert_eq!(galley.text(), "A\u{1f600}");
        assert!(galley.size().x.is_finite() && galley.size().x > 0.0);
    }

    fn font_context(fonts: FontDefinitions) -> egui::Context {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);
        ctx.run_ui(RawInput::default(), |_| {})
            .drop_without_applying_deltas();
        ctx
    }
}
