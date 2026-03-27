use egui::Color32;

pub const ANSI_COLORS: [Color32; 16] = [
    Color32::from_rgb(0, 0, 0),
    Color32::from_rgb(204, 0, 0),
    Color32::from_rgb(78, 154, 6),
    Color32::from_rgb(196, 160, 0),
    Color32::from_rgb(52, 101, 164),
    Color32::from_rgb(117, 80, 123),
    Color32::from_rgb(6, 152, 154),
    Color32::from_rgb(211, 215, 207),
    Color32::from_rgb(85, 87, 83),
    Color32::from_rgb(239, 41, 41),
    Color32::from_rgb(138, 226, 52),
    Color32::from_rgb(252, 233, 79),
    Color32::from_rgb(114, 159, 207),
    Color32::from_rgb(173, 127, 168),
    Color32::from_rgb(52, 226, 226),
    Color32::from_rgb(238, 238, 236),
];

pub fn indexed_to_egui(idx: u8) -> Color32 {
    match idx {
        0..=15 => ANSI_COLORS[idx as usize],
        16..=231 => {
            let idx = idx - 16;
            let r_idx = idx / 36;
            let g_idx = (idx % 36) / 6;
            let b_idx = idx % 6;
            let r = if r_idx > 0 { r_idx * 40 + 55 } else { 0 };
            let g = if g_idx > 0 { g_idx * 40 + 55 } else { 0 };
            let b = if b_idx > 0 { b_idx * 40 + 55 } else { 0 };
            Color32::from_rgb(r, g, b)
        }
        232..=255 => {
            let gray = (idx - 232) * 10 + 8;
            Color32::from_rgb(gray, gray, gray)
        }
    }
}

pub fn brighten(color: Color32) -> Color32 {
    let brighten = |channel: u8| ((channel as u16 * 4 / 3).min(255)) as u8;
    Color32::from_rgb(
        brighten(color.r()),
        brighten(color.g()),
        brighten(color.b()),
    )
}

pub fn dim_color(color: Color32) -> Color32 {
    Color32::from_rgb(
        (color.r() as u16 * 2 / 3) as u8,
        (color.g() as u16 * 2 / 3) as u8,
        (color.b() as u16 * 2 / 3) as u8,
    )
}
