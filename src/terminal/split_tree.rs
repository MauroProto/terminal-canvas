//! Árbol de splits de un panel de terminal (P2.11, T1).
//!
//! Modelo puro y serializable: un nodo es una hoja (una sesión) o un split con
//! un eje, un ratio 0..=1 y dos hijos. Las operaciones (split/close/focus_next/
//! layout) son funciones sobre el árbol, testeables sin montar un terminal.

use egui::{pos2, Rect};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type LeafId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    /// Divide en columnas (izquierda/derecha).
    Horizontal,
    /// Divide en filas (arriba/abajo).
    Vertical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SplitNode {
    Leaf {
        id: LeafId,
    },
    Split {
        axis: Axis,
        /// Fracción del espacio que se lleva el primer hijo, clampada a 0..=1.
        ratio: f32,
        first: Box<SplitNode>,
        second: Box<SplitNode>,
    },
}

/// Rect asignado a una hoja más el divisor que la separa de su hermano.
#[derive(Debug, Clone, PartialEq)]
pub struct LeafLayout {
    pub id: LeafId,
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DividerHit {
    pub axis: Axis,
    pub rect: Rect,
    /// Camino para actualizar el ratio al arrastrar.
    pub path: Vec<usize>,
}

impl SplitNode {
    pub fn leaf(id: LeafId) -> Self {
        Self::Leaf { id }
    }

    fn clamp_ratio(ratio: f32) -> f32 {
        ratio.clamp(0.05, 0.95)
    }

    /// Hojas en orden DFS estable (el orden de foco).
    pub fn leaves(&self) -> Vec<LeafId> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves(&self, out: &mut Vec<LeafId>) {
        match self {
            Self::Leaf { id } => out.push(*id),
            Self::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            }
        }
    }

    /// Divide `leaf` en un split con una hoja nueva. Devuelve el id nuevo.
    pub fn split(&mut self, leaf: LeafId, axis: Axis) -> Option<LeafId> {
        let new_id = Uuid::new_v4();
        let replaced = self.replace_leaf_with(leaf, &|old| SplitNode::Split {
            axis,
            ratio: 0.5,
            first: Box::new(old),
            second: Box::new(SplitNode::leaf(new_id)),
        });
        if replaced.is_some() {
            if let Some(node) = replaced {
                *self = node;
            }
            Some(new_id)
        } else {
            None
        }
    }

    /// Devuelve el nuevo subárbol si `leaf` estaba en este nodo (y fue
    /// reemplazada por `f(old)`), o None si no estaba.
    fn replace_leaf_with(
        &mut self,
        leaf: LeafId,
        f: &impl Fn(SplitNode) -> SplitNode,
    ) -> Option<SplitNode> {
        match self {
            Self::Leaf { id } if *id == leaf => Some(f(SplitNode::leaf(leaf))),
            Self::Split { first, second, .. } => {
                if let Some(repl) = first.replace_leaf_with(leaf, f) {
                    **first = repl;
                    return Some(self.clone());
                }
                if let Some(repl) = second.replace_leaf_with(leaf, f) {
                    **second = repl;
                    return Some(self.clone());
                }
                None
            }
            _ => None,
        }
    }

    /// Cierra `leaf`. Si queda un solo hijo, colapsa el split. Devuelve el id
    /// de la hoja que debe recibir el foco después, si alguna.
    pub fn close(&mut self, leaf: LeafId) -> CloseResult {
        let mut closed = false;
        let replacement = self.close_inner(leaf, &mut closed);
        if let Some(node) = replacement {
            *self = node;
        }
        if !closed {
            return CloseResult::NotFound;
        }
        // Si el árbol entero colapsó a la tumba, quedó vacío.
        if is_tombstone(self) {
            return CloseResult::Emptied;
        }
        let leaves = self.leaves();
        if leaves.is_empty() {
            CloseResult::Emptied
        } else {
            CloseResult::Closed {
                next_focus: leaves.first().copied(),
            }
        }
    }

    /// Devuelve el nodo que debe reemplazar al actual tras cerrar `leaf`
    /// (colapsando splits que quedan con un solo hijo), o None si no cambia.
    fn close_inner(&mut self, leaf: LeafId, closed: &mut bool) -> Option<SplitNode> {
        match self {
            Self::Leaf { id } if *id == leaf => {
                *closed = true;
                // Se marca como vacío; el padre lo colapsa.
                Some(SplitNode::Leaf { id: Uuid::nil() })
            }
            Self::Split { first, second, .. } => {
                if let Some(repl) = first.close_inner(leaf, closed) {
                    if is_tombstone(&repl) {
                        return Some((**second).clone());
                    }
                    **first = repl;
                }
                if let Some(repl) = second.close_inner(leaf, closed) {
                    if is_tombstone(&repl) {
                        return Some((**first).clone());
                    }
                    **second = repl;
                }
                None
            }
            _ => None,
        }
    }

    /// Siguiente hoja en orden DFS después de `current` (con wrap).
    pub fn focus_next(&self, current: LeafId) -> Option<LeafId> {
        let leaves = self.leaves();
        if leaves.is_empty() {
            return None;
        }
        let pos = leaves.iter().position(|id| *id == current);
        let next = match pos {
            Some(p) => (p + 1) % leaves.len(),
            None => 0,
        };
        leaves.get(next).copied()
    }

    /// Actualiza el ratio del split en `path` (0=first,1=second no se usa para
    /// ratio; el path identifica splits anidados).
    pub fn set_ratio(&mut self, path: &[usize], ratio: f32) {
        let mut node = self;
        for &index in path {
            match node {
                Self::Split { first, second, .. } => {
                    node = if index == 0 { first.as_mut() } else { second.as_mut() };
                }
                _ => return,
            }
        }
        if let Self::Split { ratio: r, .. } = node {
            *r = Self::clamp_ratio(ratio);
        }
    }

    /// Asigna rects a las hojas recorriendo el árbol (DFS) dentro de `rect`.
    pub fn layout(&self, rect: Rect) -> (Vec<LeafLayout>, Vec<DividerHit>) {
        let mut leaves = Vec::new();
        let mut dividers = Vec::new();
        self.layout_inner(rect, Vec::new(), &mut leaves, &mut dividers);
        (leaves, dividers)
    }

    fn layout_inner(
        &self,
        rect: Rect,
        path: Vec<usize>,
        leaves: &mut Vec<LeafLayout>,
        dividers: &mut Vec<DividerHit>,
    ) {
        match self {
            Self::Leaf { id } => leaves.push(LeafLayout { id: *id, rect }),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let ratio = Self::clamp_ratio(*ratio);
                let (first_rect, second_rect, divider_rect) = match axis {
                    Axis::Horizontal => {
                        let w = rect.width();
                        let split_x = rect.left() + w * ratio;
                        (
                            Rect::from_min_max(rect.min, pos2(split_x, rect.bottom())),
                            Rect::from_min_max(pos2(split_x, rect.top()), rect.max),
                            Rect::from_min_max(
                                pos2(split_x - 3.0, rect.top()),
                                pos2(split_x + 3.0, rect.bottom()),
                            ),
                        )
                    }
                    Axis::Vertical => {
                        let h = rect.height();
                        let split_y = rect.top() + h * ratio;
                        (
                            Rect::from_min_max(rect.min, pos2(rect.right(), split_y)),
                            Rect::from_min_max(pos2(rect.left(), split_y), rect.max),
                            Rect::from_min_max(
                                pos2(rect.left(), split_y - 3.0),
                                pos2(rect.right(), split_y + 3.0),
                            ),
                        )
                    }
                };
                dividers.push(DividerHit {
                    axis: *axis,
                    rect: divider_rect,
                    path: path.clone(),
                });
                let mut p0 = path.clone();
                p0.push(0);
                first.layout_inner(first_rect, p0, leaves, dividers);
                let mut p1 = path;
                p1.push(1);
                second.layout_inner(second_rect, p1, leaves, dividers);
            }
        }
    }

    /// Tamaño total del árbol (hojas).
    pub fn leaf_count(&self) -> usize {
        self.leaves().len()
    }

    pub fn contains(&self, leaf: LeafId) -> bool {
        self.leaves().contains(&leaf)
    }
}

fn is_tombstone(node: &SplitNode) -> bool {
    matches!(node, SplitNode::Leaf { id } if *id == Uuid::nil())
}

#[derive(Debug, PartialEq, Eq)]
pub enum CloseResult {
    Closed { next_focus: Option<LeafId> },
    Emptied,
    NotFound,
}

#[cfg(test)]
mod tests {
    use super::{Axis, CloseResult, SplitNode};
    use egui::{pos2, Rect};
    use uuid::Uuid;

    fn rect() -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0))
    }

    #[test]
    fn a_single_leaf_layouts_to_the_full_rect() {
        let id = Uuid::new_v4();
        let node = SplitNode::leaf(id);
        let (leaves, dividers) = node.layout(rect());
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].id, id);
        assert_eq!(leaves[0].rect, rect());
        assert!(dividers.is_empty());
    }

    #[test]
    fn split_creates_two_leaves_in_dfs_order() {
        let a = Uuid::new_v4();
        let mut node = SplitNode::leaf(a);
        let b = node.split(a, Axis::Horizontal).unwrap();
        assert_eq!(node.leaves(), vec![a, b]);
        assert_eq!(node.leaf_count(), 2);
    }

    #[test]
    fn closing_the_only_leaf_empties() {
        let a = Uuid::new_v4();
        let mut node = SplitNode::leaf(a);
        match node.close(a) {
            CloseResult::Emptied => {}
            other => panic!("expected Emptied, got {other:?}"),
        }
    }

    #[test]
    fn closing_one_leaf_collapses_to_the_sibling() {
        let a = Uuid::new_v4();
        let mut node = SplitNode::leaf(a);
        let b = node.split(a, Axis::Horizontal).unwrap();
        match node.close(a) {
            CloseResult::Closed { next_focus } => {
                assert_eq!(next_focus, Some(b));
            }
            other => panic!("expected Closed, got {other:?}"),
        }
        assert_eq!(node.leaves(), vec![b], "colapsa al hermano");
    }

    #[test]
    fn closing_a_missing_leaf_is_not_found() {
        let a = Uuid::new_v4();
        let mut node = SplitNode::leaf(a);
        assert_eq!(node.close(Uuid::new_v4()), CloseResult::NotFound);
    }

    #[test]
    fn focus_next_wraps_in_dfs_order() {
        let a = Uuid::new_v4();
        let mut node = SplitNode::leaf(a);
        let b = node.split(a, Axis::Horizontal).unwrap();
        let c = node.split(b, Axis::Vertical).unwrap();
        // DFS: a, b, c
        assert_eq!(node.leaves(), vec![a, b, c]);
        assert_eq!(node.focus_next(a), Some(b));
        assert_eq!(node.focus_next(b), Some(c));
        assert_eq!(node.focus_next(c), Some(a), "wrap");
    }

    #[test]
    fn ratios_are_clamped() {
        let a = Uuid::new_v4();
        let mut node = SplitNode::leaf(a);
        node.split(a, Axis::Horizontal).unwrap();
        node.set_ratio(&[], 5.0);
        if let SplitNode::Split { ratio, .. } = &node {
            assert!(*ratio <= 0.95, "clamp superior: {ratio}");
        } else {
            panic!("expected split");
        }
        node.set_ratio(&[], -1.0);
        if let SplitNode::Split { ratio, .. } = &node {
            assert!(*ratio >= 0.05, "clamp inferior: {ratio}");
        }
    }

    #[test]
    fn layout_partitions_the_rect_without_overlap() {
        let a = Uuid::new_v4();
        let mut node = SplitNode::leaf(a);
        node.split(a, Axis::Horizontal).unwrap();
        let (leaves, dividers) = node.layout(rect());
        assert_eq!(leaves.len(), 2);
        assert_eq!(dividers.len(), 1);
        let ra = &leaves[0].rect;
        let rb = &leaves[1].rect;
        // Mitades: a la izquierda, b a la derecha (ratio 0.5).
        assert!((ra.right() - 50.0).abs() < 0.01, "got {ra:?}");
        assert!((rb.left() - 50.0).abs() < 0.01, "got {rb:?}");
        assert!(ra.width() + rb.width() <= 100.0 + 0.01);
    }

    #[test]
    fn serde_round_trip_preserves_the_tree() {
        let a = Uuid::new_v4();
        let mut node = SplitNode::leaf(a);
        let b = node.split(a, Axis::Vertical).unwrap();
        let json = serde_json::to_string(&node).unwrap();
        let back: SplitNode = serde_json::from_str(&json).unwrap();
        assert_eq!(back.leaves(), vec![a, b]);
    }
}
