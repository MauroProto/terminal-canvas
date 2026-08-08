//! Tokens de color de la UI, alineados con el style guide de Orca
//! (`docs/STYLEGUIDE.md` de `stablyai/orca`): identidad **monocroma y
//! callada**, con el color reservado para estado. Los valores son los de su
//! tema oscuro.
//!
//! El criterio de fondo, que es lo que hay que respetar al agregar tokens: la
//! app se pasa la vida hospedando la salida de otras herramientas (shells,
//! agentes, diffs), así que su propio chrome tiene que enmarcar y correrse del
//! medio. Antes de inventar un color nuevo, buscá si `DIM`/`LINE`/`HOVER`
//! alcanzan.
//!
//! Los tokens vienen en pares superficie/texto: pintá el texto con el
//! foreground que corresponde a la superficie de abajo, o se rompe el
//! contraste.

use egui::Color32;

// --- Superficies, de más profunda a más levantada ---

/// Canvas de la app: el fondo sobre el que flota todo lo demás.
pub const INK: Color32 = Color32::from_rgb(0x0a, 0x0a, 0x0a);

/// Superficie levantada del canvas: cuerpo de las ventanas de terminal,
/// diálogos, popovers. Equivale a `card`/`popover` de Orca.
pub const SURFACE: Color32 = Color32::from_rgb(0x17, 0x17, 0x17);

/// Chrome secundario apoyado sobre `SURFACE`: titlebars, badges, chips en
/// reposo. Equivale a `secondary`/`muted`.
pub const RAISED: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);

/// Fondo de hover para filas de lista y botones ghost.
pub const HOVER: Color32 = Color32::from_rgb(0x35, 0x35, 0x35);

/// Fondo de la fila seleccionada o "actual". Equivale a `accent`: es el token
/// de selección de la app, no inventes otro ni hardcodees un gris.
pub const FOCUS: Color32 = Color32::from_rgb(0x40, 0x40, 0x40);

// --- Líneas ---

/// Hairline para divisores, bordes de tarjeta y de input: blanco al 7%, no un
/// gris opaco. Va con alfa a propósito, para que se apoye en la superficie que
/// tenga debajo en vez de fijar un gris que sólo cierra sobre un fondo.
///
/// Si el borde se ve antes que el contenido que separa, está mal.
pub const LINE: Color32 = Color32::from_rgba_premultiplied(18, 18, 18, 18);

/// Halo de foco y selección activa. Es el único realce más fuerte que una
/// hairline; si algo necesita más peso que esto, casi siempre el problema es
/// la jerarquía, no el color.
pub const RING: Color32 = Color32::from_rgb(0x73, 0x73, 0x73);

// --- Texto ---

/// Texto principal sobre cualquiera de las superficies de arriba.
pub const TEXT_STRONG: Color32 = Color32::from_rgb(0xfa, 0xfa, 0xfa);

/// Cuerpo de texto de menor jerarquía que un título, pero que igual se lee
/// como contenido.
pub const TEXT: Color32 = Color32::from_rgb(0xd4, 0xd4, 0xd4);

/// Texto desenfatizado: paths, captions, placeholders, chrome deshabilitado.
/// No lo uses para contenido que el usuario tenga que leer sí o sí.
pub const DIM: Color32 = Color32::from_rgb(0xa1, 0xa1, 0xa1);

/// Blanco puro. Reservado para destellos momentáneos (el flash de bell) y para
/// el cursor del terminal, donde el punto es justamente pegar el salto.
pub const WHITE: Color32 = Color32::from_rgb(255, 255, 255);
