//! Métricas base de la grilla del terminal. El tamaño de fuente base es
//! configurable (`config.toml` → `[terminal] font_size`); todo el pipeline
//! (renderer, layout, scrollbar, scroll-wheel) lee de acá para mantenerse
//! sincronizado.

use std::sync::atomic::{AtomicU32, Ordering};

pub const DEFAULT_FONT_SIZE: f32 = 15.0;
pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 32.0;
/// Por debajo de este tamaño (ya escalado por zoom) el texto no se dibuja:
/// los glifos sub-3px son ruido y el layout de galleys no aporta nada.
pub const MIN_TEXT_RENDER_FONT_SIZE: f32 = 3.4;
pub const CELL_WIDTH_FACTOR: f32 = 0.6;
pub const CELL_HEIGHT_FACTOR: f32 = 1.25;
pub const PAD_X: f32 = 10.0;
pub const PAD_Y: f32 = 6.0;

/// Tamaño de fuente base en bits de `f32`, para un `AtomicU32` lock-free.
static BASE_FONT_SIZE_BITS: AtomicU32 = AtomicU32::new(DEFAULT_FONT_SIZE.to_bits());

/// Fija el tamaño de fuente base. Solo la primera llamada efectiva importa
/// (la app lo setea una vez al arrancar desde la config); posteriores son
/// ignoradas acá — para cambios en vivo usar `set_base_font_size`.
pub fn install_base_font_size(size: f32) {
    let clamped = clamp_font_size(size);
    // Solo instalamos si sigue en el default: primera config gana.
    let _ = BASE_FONT_SIZE_BITS.compare_exchange(
        DEFAULT_FONT_SIZE.to_bits(),
        clamped.to_bits(),
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
}

/// Cambia el tamaño de fuente base en vivo (desde el diálogo de
/// configuración). A diferencia de `install_base_font_size`, siempre aplica.
pub fn set_base_font_size(size: f32) {
    BASE_FONT_SIZE_BITS.store(clamp_font_size(size).to_bits(), Ordering::Relaxed);
}

pub fn clamp_font_size(size: f32) -> f32 {
    let size = if size.is_finite() {
        size
    } else {
        DEFAULT_FONT_SIZE
    };
    size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

pub fn base_font_size() -> f32 {
    f32::from_bits(BASE_FONT_SIZE_BITS.load(Ordering::Relaxed))
}

/// Puntos de desplazamiento equivalentes a una línea de terminal con la
/// métrica base vigente. Acá (y no en `input.rs`) para que el harness de
/// tests de runtime —que monta `input.rs` sin el renderer— y el renderer
/// compartan la misma fuente de verdad.
pub fn scroll_points_per_line() -> f32 {
    base_font_size() * CELL_HEIGHT_FACTOR
}

#[cfg(test)]
mod tests {
    use super::{base_font_size, clamp_font_size, DEFAULT_FONT_SIZE, MAX_FONT_SIZE, MIN_FONT_SIZE};

    // Estos tests NO mutan el global `BASE_FONT_SIZE_BITS`: otros tests del
    // crate (scroll, geometría de paneles) asumen el default 15.0 y corren
    // en hilos paralelos. La instalación se cubre vía la función pura.

    #[test]
    fn base_font_size_defaults_to_constant() {
        assert_eq!(base_font_size(), DEFAULT_FONT_SIZE);
    }

    #[test]
    fn clamp_font_size_bounds_out_of_range_values() {
        assert_eq!(clamp_font_size(999.0), MAX_FONT_SIZE);
        assert_eq!(clamp_font_size(0.5), MIN_FONT_SIZE);
        assert_eq!(clamp_font_size(f32::NAN), DEFAULT_FONT_SIZE);
        assert_eq!(clamp_font_size(f32::INFINITY), DEFAULT_FONT_SIZE);
    }

    #[test]
    fn clamp_font_size_keeps_valid_values() {
        assert_eq!(clamp_font_size(13.0), 13.0);
        assert_eq!(clamp_font_size(MIN_FONT_SIZE), MIN_FONT_SIZE);
        assert_eq!(clamp_font_size(MAX_FONT_SIZE), MAX_FONT_SIZE);
    }
}
