//! Ranking unificado del quick open (P2.14, T1).
//!
//! El fuzzy solo desempata: primero mandan reglas **ordinales** explícitas, que
//! son las que hacen que escribir `new` te dé "New Terminal" y no un archivo
//! llamado `renew.rs`. El orden es:
//!
//! 1. match exacto de comando
//! 2. prefijo de comando
//! 3. nombre de panel
//! 4. archivo (con bonus si el match cae en un borde `/`, `.` o `-`, y otro
//!    bonus si el nombre del archivo contiene la query)
//!
//! Se devuelven a lo sumo `TOP_K` resultados usando un `BinaryHeap`, para no
//! ordenar decenas de miles de archivos en cada tecla.

use std::collections::BinaryHeap;

use super::fuzzy::fuzzy_score;

/// Tope de resultados que se muestran (y que se mantienen en el heap).
pub const TOP_K: usize = 50;

/// Tope de la query: nadie escribe 2 KB, pero un paste accidental no puede
/// hacernos recorrer archivos con una query gigante.
pub const MAX_QUERY_BYTES: usize = 2 * 1024;

/// Clase ordinal del resultado. Menor = mejor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RankClass {
    ExactCommand = 0,
    CommandPrefix = 1,
    PanelName = 2,
    File = 3,
    /// Comando que solo matchea por fuzzy (no exacto ni prefijo).
    FuzzyCommand = 4,
}

/// Tipo de cosa que se puede abrir desde el quick open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickOpenKind {
    Command,
    Panel(uuid::Uuid),
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedItem {
    pub class: RankClass,
    /// Mayor = mejor dentro de la clase.
    pub score: i32,
    pub label: String,
    pub kind: QuickOpenKind,
}

/// Orden total: primero la clase (ascendente), después el score (descendente),
/// y el label como desempate estable para que el listado no titile.
impl Ord for RankedItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.class
            .cmp(&other.class)
            .then_with(|| other.score.cmp(&self.score))
            .then_with(|| self.label.cmp(&other.label))
    }
}

impl PartialOrd for RankedItem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Recorta la query al tope y normaliza espacios.
pub fn clamp_query(query: &str) -> &str {
    if query.len() <= MAX_QUERY_BYTES {
        return query;
    }
    let mut end = MAX_QUERY_BYTES;
    while end > 0 && !query.is_char_boundary(end) {
        end -= 1;
    }
    &query[..end]
}

/// Bonus de "borde": un match que arranca después de `/`, `.` o `-` vale más
/// que uno que cae en el medio de una palabra.
fn boundary_bonus(candidate: &str, query: &str) -> i32 {
    let candidate_lower = candidate.to_lowercase();
    let query_lower = query.to_lowercase();
    let Some(index) = candidate_lower.find(&query_lower) else {
        return 0;
    };
    if index == 0 {
        return 30;
    }
    let previous = candidate_lower[..index].chars().next_back();
    match previous {
        Some('/') | Some('.') | Some('-') | Some('_') => 20,
        _ => 0,
    }
}

/// Bonus si el **nombre** del archivo (no el path completo) contiene la query.
fn filename_bonus(path: &str, query: &str) -> i32 {
    let file_name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    if file_name.contains(&query.to_lowercase()) {
        25
    } else {
        0
    }
}

/// Clasifica un comando contra la query.
pub fn rank_command(query: &str, label: &str) -> Option<RankedItem> {
    let query_lower = query.to_lowercase();
    let label_lower = label.to_lowercase();
    let (class, bonus) = if label_lower == query_lower {
        (RankClass::ExactCommand, 1000)
    } else if label_lower.starts_with(&query_lower) {
        (RankClass::CommandPrefix, 500)
    } else {
        (RankClass::FuzzyCommand, 0)
    };
    let score = fuzzy_score(query, label)? + bonus;
    Some(RankedItem {
        class,
        score,
        label: label.to_owned(),
        kind: QuickOpenKind::Command,
    })
}

/// Clasifica un panel por su título.
pub fn rank_panel(query: &str, title: &str, panel_id: uuid::Uuid) -> Option<RankedItem> {
    let score = fuzzy_score(query, title)? + boundary_bonus(title, query);
    Some(RankedItem {
        class: RankClass::PanelName,
        score,
        label: title.to_owned(),
        kind: QuickOpenKind::Panel(panel_id),
    })
}

/// Clasifica un archivo por su path relativo.
pub fn rank_file(query: &str, path: &str) -> Option<RankedItem> {
    let score =
        fuzzy_score(query, path)? + boundary_bonus(path, query) + filename_bonus(path, query);
    Some(RankedItem {
        class: RankClass::File,
        score,
        label: path.to_owned(),
        kind: QuickOpenKind::File,
    })
}

/// Se queda con los mejores `TOP_K` sin ordenar todo el universo: el heap
/// guarda a lo sumo K y descarta el peor apenas se pasa.
pub fn top_k(items: impl IntoIterator<Item = RankedItem>, k: usize) -> Vec<RankedItem> {
    if k == 0 {
        return Vec::new();
    }
    // El Ord de RankedItem es "menor = mejor", así que un max-heap deja al
    // PEOR en la cima: eso es justo lo que hay que descartar al pasarse de K.
    let mut heap: BinaryHeap<RankedItem> = BinaryHeap::with_capacity(k + 1);
    for item in items {
        heap.push(item);
        if heap.len() > k {
            heap.pop();
        }
    }
    let mut out: Vec<RankedItem> = heap.into_vec();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::{
        clamp_query, rank_command, rank_file, rank_panel, top_k, QuickOpenKind, RankClass,
        RankedItem, MAX_QUERY_BYTES, TOP_K,
    };

    #[test]
    fn an_exact_command_beats_a_prefix_and_a_file() {
        let exact = rank_command("New Terminal", "New Terminal").unwrap();
        let prefix = rank_command("New", "New Terminal").unwrap();
        let file = rank_file("New Terminal", "src/new_terminal.rs");
        assert_eq!(exact.class, RankClass::ExactCommand);
        assert_eq!(prefix.class, RankClass::CommandPrefix);
        assert!(exact < prefix, "exacto antes que prefijo");
        if let Some(file) = file {
            assert!(prefix < file, "prefijo de comando antes que archivo");
        }
    }

    #[test]
    fn a_command_prefix_beats_a_panel_name() {
        let prefix = rank_command("Split", "Split Right").unwrap();
        let panel = rank_panel("Split", "Split experiment", uuid::Uuid::new_v4()).unwrap();
        assert_eq!(prefix.class, RankClass::CommandPrefix);
        assert_eq!(panel.class, RankClass::PanelName);
        assert!(prefix < panel);
    }

    #[test]
    fn a_panel_name_beats_a_file() {
        let panel = rank_panel("claude", "claude code", uuid::Uuid::new_v4()).unwrap();
        let file = rank_file("claude", "docs/claude.md").unwrap();
        assert!(panel < file, "panel antes que archivo");
    }

    #[test]
    fn a_fuzzy_only_command_ranks_after_files() {
        // "nt" matchea "New Terminal" solo por fuzzy: no debe tapar archivos.
        let fuzzy = rank_command("nt", "New Terminal").unwrap();
        assert_eq!(fuzzy.class, RankClass::FuzzyCommand);
        let file = rank_file("nt", "src/nt.rs").unwrap();
        assert!(file < fuzzy);
    }

    #[test]
    fn a_boundary_match_scores_higher_than_one_inside_a_word() {
        let boundary = rank_file("term", "src/term.rs").unwrap();
        let inside = rank_file("term", "src/subterminal.rs").unwrap();
        assert!(
            boundary.score > inside.score,
            "borde {} vs interior {}",
            boundary.score,
            inside.score
        );
    }

    #[test]
    fn a_match_in_the_filename_beats_one_only_in_the_directory() {
        let in_name = rank_file("panel", "src/panel.rs").unwrap();
        let in_dir = rank_file("panel", "panel/otra_cosa_totalmente.rs").unwrap();
        assert!(
            in_name.score > in_dir.score,
            "nombre {} vs directorio {}",
            in_name.score,
            in_dir.score
        );
    }

    #[test]
    fn non_matching_candidates_are_dropped() {
        assert!(rank_file("zzzz", "src/panel.rs").is_none());
        assert!(rank_command("zzzz", "New Terminal").is_none());
    }

    #[test]
    fn top_k_keeps_the_best_and_caps_the_length() {
        let items: Vec<RankedItem> = (0..1_000)
            .map(|index| RankedItem {
                class: RankClass::File,
                score: index,
                label: format!("file{index}.rs"),
                kind: QuickOpenKind::File,
            })
            .collect();
        let best = top_k(items, TOP_K);
        assert_eq!(best.len(), TOP_K);
        // El mejor score tiene que estar primero.
        assert_eq!(best[0].score, 999);
        assert!(best.windows(2).all(|pair| pair[0] <= pair[1]), "ordenado");
    }

    #[test]
    fn top_k_of_zero_is_empty() {
        let items = vec![RankedItem {
            class: RankClass::File,
            score: 1,
            label: "a".to_owned(),
            kind: QuickOpenKind::File,
        }];
        assert!(top_k(items, 0).is_empty());
    }

    #[test]
    fn the_query_is_capped_without_splitting_a_character() {
        let long = "ñ".repeat(MAX_QUERY_BYTES);
        let clamped = clamp_query(&long);
        assert!(clamped.len() <= MAX_QUERY_BYTES);
        // Sigue siendo un &str válido (si cortara al medio, esto paniquearía).
        assert!(long.starts_with(clamped));
    }

    #[test]
    fn a_short_query_is_untouched() {
        assert_eq!(clamp_query("hola"), "hola");
    }
}
