//! Avisos efímeros ("toasts") apilados en la esquina inferior derecha del
//! canvas. Sirven para confirmar acciones que no tienen otro feedback visible
//! (guardar configuración, exportar scrollback, broadcast a N terminales).
//!
//! La lógica de expiración y de tope de la pila es pura y está testeada; el
//! render sólo dibuja lo que `visible_toasts` decide.

use std::time::{Duration, Instant};

use egui::{vec2, Color32, FontId, Rect};

use crate::theme::colors as palette;

use super::TerminalApp;

/// Tope de toasts simultáneos: más que esto tapa el canvas, así que los
/// viejos se descartan aunque no hayan expirado.
const MAX_TOASTS: usize = 4;
/// Cuánto vive un toast informativo antes de desaparecer.
const INFO_TTL: Duration = Duration::from_secs(4);
/// Los errores viven más: el usuario necesita tiempo para leerlos.
const ERROR_TTL: Duration = Duration::from_secs(8);
/// Duración del fundido de salida, descontada del final del TTL.
const FADE: Duration = Duration::from_millis(450);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToastKind {
    Success,
    Error,
}

impl ToastKind {
    fn ttl(self) -> Duration {
        match self {
            Self::Success => INFO_TTL,
            Self::Error => ERROR_TTL,
        }
    }

    fn accent(self) -> Color32 {
        match self {
            Self::Success => Color32::from_rgb(126, 222, 152),
            Self::Error => Color32::from_rgb(238, 130, 130),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Toast {
    pub(super) kind: ToastKind,
    pub(super) text: String,
    pub(super) born: Instant,
}

impl Toast {
    /// Fracción de opacidad en `now`: 1.0 mientras está fresco y bajando a 0.0
    /// en los últimos `FADE` de vida. `None` cuando ya expiró.
    fn opacity_at(&self, now: Instant) -> Option<f32> {
        let ttl = self.kind.ttl();
        let age = now.saturating_duration_since(self.born);
        if age >= ttl {
            return None;
        }
        let remaining = ttl - age;
        if remaining >= FADE {
            return Some(1.0);
        }
        Some(remaining.as_secs_f32() / FADE.as_secs_f32())
    }
}

/// Pila de avisos. `push` mantiene el orden de llegada y recorta al tope.
#[derive(Debug, Default)]
pub(super) struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub(super) fn push(&mut self, kind: ToastKind, text: impl Into<String>, now: Instant) {
        self.items.push(Toast {
            kind,
            text: text.into(),
            born: now,
        });
        // Primero soltamos lo ya expirado; sólo si aún sobran recortamos por
        // tope, para no descartar avisos que el usuario todavía no leyó.
        self.retain_live(now);
        let overflow = self.items.len().saturating_sub(MAX_TOASTS);
        if overflow > 0 {
            self.items.drain(..overflow);
        }
    }

    fn retain_live(&mut self, now: Instant) {
        self.items
            .retain(|toast| toast.opacity_at(now).is_some_and(|alpha| alpha > 0.0));
    }

    /// Avisos a dibujar en `now`, del más viejo al más nuevo, con su opacidad.
    /// Purga de paso los expirados.
    fn visible(&mut self, now: Instant) -> Vec<(Toast, f32)> {
        self.retain_live(now);
        self.items
            .iter()
            .filter_map(|toast| toast.opacity_at(now).map(|alpha| (toast.clone(), alpha)))
            .collect()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl TerminalApp {
    pub(super) fn toast_success(&mut self, text: impl Into<String>) {
        self.toasts.push(ToastKind::Success, text, Instant::now());
    }

    pub(super) fn toast_error(&mut self, text: impl Into<String>) {
        self.toasts.push(ToastKind::Error, text, Instant::now());
    }

    pub(super) fn show_toasts(&mut self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        let now = Instant::now();
        let visible = self.toasts.visible(now);
        if visible.is_empty() {
            return;
        }
        // Mientras haya avisos vivos hay que seguir pintando para que el
        // fundido avance incluso sin input del usuario.
        ctx.request_repaint();

        let screen = ctx.screen_rect();
        let font = FontId::proportional(12.0);
        let width = 320.0;
        let margin = 18.0;
        let mut bottom = screen.max.y - margin;

        // De abajo hacia arriba: el más nuevo queda pegado al borde inferior.
        for (toast, alpha) in visible.iter().rev() {
            let galley = ctx.fonts(|fonts| {
                fonts.layout(
                    toast.text.clone(),
                    font.clone(),
                    palette::TEXT_STRONG,
                    width - 40.0,
                )
            });
            let height = (galley.size().y + 22.0).max(40.0);
            let rect = Rect::from_min_size(
                egui::pos2(screen.max.x - margin - width, bottom - height),
                vec2(width, height),
            );
            bottom = rect.min.y - 8.0;

            let fade = |color: Color32| color.gamma_multiply(*alpha);
            egui::Area::new(egui::Id::new(("toast", toast.born, &toast.text)))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.min)
                .interactable(false)
                .show(ctx, |ui| {
                    let painter = ui.painter();
                    painter.rect_filled(rect, 10.0, fade(palette::RAISED));
                    painter.rect_stroke(rect, 10.0, egui::Stroke::new(1.0, fade(palette::LINE)));
                    // Barra de acento a la izquierda: color según el tipo.
                    let accent = Rect::from_min_size(rect.min, vec2(3.0, rect.height()));
                    painter.rect_filled(accent, 2.0, fade(toast.kind.accent()));
                    painter.galley(
                        rect.min + vec2(16.0, (rect.height() - galley.size().y) * 0.5),
                        galley,
                        fade(palette::TEXT_STRONG),
                    );
                    // Reservamos el área para que egui no colapse el Area.
                    ui.allocate_space(rect.size());
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Toast, ToastKind};
    use super::{Toasts, ERROR_TTL, FADE, INFO_TTL, MAX_TOASTS};

    fn toast(kind: ToastKind, born: Instant) -> Toast {
        Toast {
            kind,
            text: "x".to_owned(),
            born,
        }
    }

    #[test]
    fn fresh_toast_is_fully_opaque() {
        let now = Instant::now();
        assert_eq!(toast(ToastKind::Success, now).opacity_at(now), Some(1.0));
    }

    #[test]
    fn toast_fades_out_over_the_last_stretch_of_its_life() {
        let born = Instant::now();
        let toast = toast(ToastKind::Success, born);
        // A mitad del fundido la opacidad ronda 0.5.
        let mid_fade = born + INFO_TTL - FADE / 2;
        let alpha = toast.opacity_at(mid_fade).expect("still alive");
        assert!(alpha > 0.3 && alpha < 0.7, "unexpected alpha {alpha}");
    }

    #[test]
    fn expired_toast_has_no_opacity() {
        let born = Instant::now();
        let expired = born + INFO_TTL + Duration::from_millis(1);
        assert_eq!(toast(ToastKind::Success, born).opacity_at(expired), None);
    }

    #[test]
    fn errors_outlive_success_notices() {
        let born = Instant::now();
        let after_info = born + INFO_TTL + Duration::from_millis(1);
        assert_eq!(toast(ToastKind::Success, born).opacity_at(after_info), None);
        assert!(toast(ToastKind::Error, born)
            .opacity_at(after_info)
            .is_some());
        assert!(ERROR_TTL > INFO_TTL);
    }

    #[test]
    fn stack_is_capped_dropping_the_oldest_first() {
        let now = Instant::now();
        let mut toasts = Toasts::default();
        for index in 0..MAX_TOASTS + 2 {
            toasts.push(ToastKind::Success, format!("toast {index}"), now);
        }
        let visible = toasts.visible(now);
        assert_eq!(visible.len(), MAX_TOASTS);
        // Se conservan los últimos MAX_TOASTS, en orden de llegada.
        assert_eq!(visible[0].0.text, "toast 2");
        assert_eq!(visible[MAX_TOASTS - 1].0.text, "toast 5");
    }

    #[test]
    fn expired_toasts_are_purged_before_applying_the_cap() {
        let start = Instant::now();
        let mut toasts = Toasts::default();
        for index in 0..MAX_TOASTS {
            toasts.push(ToastKind::Success, format!("old {index}"), start);
        }
        // Mucho después, un aviso nuevo no debe verse recortado por los viejos.
        let later = start + INFO_TTL + Duration::from_secs(1);
        toasts.push(ToastKind::Success, "fresh", later);
        let visible = toasts.visible(later);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].0.text, "fresh");
    }

    #[test]
    fn empty_stack_reports_empty_and_visible_is_empty() {
        let mut toasts = Toasts::default();
        assert!(toasts.is_empty());
        assert!(toasts.visible(Instant::now()).is_empty());
    }
}
