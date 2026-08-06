//! Benches de las rutas calientes (Ship-it 7.3, T2).
//!
//! Tres cosas que el usuario siente si se degradan: pintar el grid, resaltar
//! un archivo grande en el visor, y parsear un diff grande en el review.
//! `scripts/bench-compare.sh` corre esto contra el commit anterior y falla si
//! algo empeora más de 15%.

use std::sync::mpsc;

use criterion::{criterion_group, criterion_main, Criterion};

use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

use mi_terminal::orchestration::parse_unified_diff;
use mi_terminal::terminal::export::scrollback_to_ansi;
use mi_terminal::terminal::pty::EventProxy;

/// Term con ~2000 celdas de contenido real (80x25), con colores.
fn term_with_cells() -> Term<EventProxy> {
    let (tx, _rx) = mpsc::channel();
    let mut term = Term::new(
        TermConfig::default(),
        &TermSize::new(80, 25),
        EventProxy::new(tx),
    );
    let mut parser: Processor<StdSyncHandler> = Processor::new();
    let mut input = String::new();
    for row in 0..25 {
        input.push_str(&format!(
            "\x1b[3{}mfila {row:02} con texto de relleno para llenar la linea\x1b[0m\r\n",
            row % 8
        ));
    }
    parser.advance(&mut term, input.as_bytes());
    term
}

/// Diff unificado sintético de ~5000 líneas.
fn big_diff() -> String {
    let mut out = String::with_capacity(200_000);
    for file in 0..50 {
        out.push_str(&format!("diff --git a/src/f{file}.rs b/src/f{file}.rs\n"));
        out.push_str(&format!("--- a/src/f{file}.rs\n+++ b/src/f{file}.rs\n"));
        out.push_str("@@ -1,50 +1,50 @@\n");
        for line in 0..100 {
            match line % 3 {
                0 => out.push_str(&format!("+    let nuevo_{line} = {line};\n")),
                1 => out.push_str(&format!("-    let viejo_{line} = {line};\n")),
                _ => out.push_str(&format!("     let igual_{line} = {line};\n")),
            }
        }
    }
    out
}

/// Texto de ~2000 líneas de Rust para el resaltado del visor.
fn big_source() -> String {
    let mut out = String::with_capacity(120_000);
    for index in 0..2000 {
        out.push_str(&format!(
            "pub fn funcion_{index}(valor: usize) -> usize {{ // comentario {index}\n"
        ));
    }
    out
}

fn bench_render_grid(criterion: &mut Criterion) {
    let term = term_with_cells();
    criterion.bench_function("scrollback_to_ansi_2000_cells", |bencher| {
        bencher.iter(|| scrollback_to_ansi(std::hint::black_box(&term)));
    });
}

fn bench_parse_diff(criterion: &mut Criterion) {
    let diff = big_diff();
    criterion.bench_function("parse_unified_diff_5000_lines", |bencher| {
        bencher.iter(|| parse_unified_diff(std::hint::black_box(&diff)));
    });
}

fn bench_highlight(criterion: &mut Criterion) {
    let source = big_source();
    criterion.bench_function("highlight_2000_lines", |bencher| {
        bencher.iter(|| {
            mi_terminal::app::code_highlight::highlight_text(
                "grande.rs",
                std::hint::black_box(&source),
            )
        });
    });
}

criterion_group!(
    benches,
    bench_render_grid,
    bench_parse_diff,
    bench_highlight
);
criterion_main!(benches);
