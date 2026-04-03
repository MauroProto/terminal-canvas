#![allow(dead_code)]

use egui::{Key, Modifiers};

pub const VOID_SHORTCUTS: &[(Modifiers, Key)] = &[
    (
        Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        },
        Key::B,
    ),
    (
        Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        },
        Key::M,
    ),
    (
        Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        },
        Key::G,
    ),
    (
        Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        },
        Key::T,
    ),
    (
        Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        },
        Key::O,
    ),
    (
        Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        },
        Key::W,
    ),
    (
        Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        },
        Key::A,
    ),
    (
        Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        },
        Key::S,
    ),
    (
        Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        },
        Key::J,
    ),
    (
        Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        },
        Key::P,
    ),
];

pub fn is_void_shortcut(modifiers: &Modifiers, key: &Key) -> bool {
    VOID_SHORTCUTS.iter().any(|(m, k)| {
        m.ctrl == modifiers.ctrl
            && m.shift == modifiers.shift
            && m.alt == modifiers.alt
            && *k == *key
    })
}
