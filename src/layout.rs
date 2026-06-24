//! Binary space-partition tiling tree. Panes are leaves; splits are internal
//! nodes with a ratio. See docs/04-data-model.md §4.

use std::collections::HashMap;

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

use crate::ids::PaneId;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Children side by side (vertical divider).
    Col,
    /// Children stacked (horizontal divider).
    Row,
}

enum Node {
    Leaf(PaneId),
    Split {
        axis: Axis,
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

pub struct PaneInfo {
    pub id: PaneId,
    pub rect: Rect,
}

pub struct TileLayout {
    root: Node,
    pub focus: PaneId,
}

/// Serializable mirror of the tree (leaves carry the runtime pane id at save
/// time). Used by persistence; see docs/09.
#[derive(Clone, Serialize, Deserialize)]
pub enum LayoutTree {
    Leaf(u32),
    Split {
        axis: u8, // 0 = Col, 1 = Row
        ratio: f32,
        a: Box<LayoutTree>,
        b: Box<LayoutTree>,
    },
}

impl TileLayout {
    pub fn new(root: PaneId) -> Self {
        TileLayout {
            root: Node::Leaf(root),
            focus: root,
        }
    }

    pub fn to_tree(&self) -> LayoutTree {
        node_to_tree(&self.root)
    }

    /// Rebuild a layout from a saved tree, mapping old raw ids to freshly
    /// allocated panes. Returns `None` if no leaf survives.
    pub fn from_tree(
        tree: &LayoutTree,
        remap: &HashMap<u32, PaneId>,
        focus_raw: u32,
    ) -> Option<Self> {
        let root = build_node(tree, remap)?;
        let focus = remap
            .get(&focus_raw)
            .copied()
            .unwrap_or_else(|| first_leaf(&root));
        Some(TileLayout { root, focus })
    }

    pub fn len(&self) -> usize {
        count(&self.root)
    }

    /// Leaves in left-to-right / top-to-bottom order.
    pub fn leaves(&self) -> Vec<PaneId> {
        let mut v = Vec::new();
        collect_leaves(&self.root, &mut v);
        v
    }

    /// Geometry for every pane within `area`.
    pub fn panes(&self, area: Rect) -> Vec<PaneInfo> {
        let mut v = Vec::new();
        collect(&self.root, area, &mut v);
        v
    }

    /// Replace the focused leaf with a split and focus the new pane.
    pub fn split_focused(&mut self, axis: Axis, new_id: PaneId) {
        split_at(&mut self.root, self.focus, axis, new_id);
        self.focus = new_id;
    }

    /// Remove a pane, collapsing its parent split. Returns `true` if the tree
    /// is now empty.
    pub fn remove(&mut self, id: PaneId) -> bool {
        let root = std::mem::replace(&mut self.root, Node::Leaf(id));
        match remove_node(root, id) {
            Some(n) => {
                self.root = n;
                if self.focus == id {
                    self.focus = first_leaf(&self.root);
                }
                false
            }
            None => true,
        }
    }

    /// Move focus to the nearest pane in `dir` (geometric).
    pub fn focus_dir(&mut self, area: Rect, dir: Dir) {
        if let Some(id) = self.find_in_direction(area, dir) {
            self.focus = id;
        }
    }

    fn find_in_direction(&self, area: Rect, dir: Dir) -> Option<PaneId> {
        let panes = self.panes(area);
        let cur = panes.iter().find(|p| p.id == self.focus)?;
        let (cx, cy) = center(cur.rect);
        let mut best: Option<PaneId> = None;
        let mut best_d = i64::MAX;
        for p in &panes {
            if p.id == self.focus {
                continue;
            }
            let (px, py) = center(p.rect);
            let ahead = match dir {
                Dir::Right => px > cx,
                Dir::Left => px < cx,
                Dir::Down => py > cy,
                Dir::Up => py < cy,
            };
            if !ahead {
                continue;
            }
            let (along, perp) = match dir {
                Dir::Left | Dir::Right => ((px - cx).abs(), (py - cy).abs()),
                Dir::Up | Dir::Down => ((py - cy).abs(), (px - cx).abs()),
            };
            let d = along as i64 * 1000 + perp as i64;
            if d < best_d {
                best_d = d;
                best = Some(p.id);
            }
        }
        best
    }
}

fn count(node: &Node) -> usize {
    match node {
        Node::Leaf(_) => 1,
        Node::Split { a, b, .. } => count(a) + count(b),
    }
}

fn collect_leaves(node: &Node, out: &mut Vec<PaneId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Split { a, b, .. } => {
            collect_leaves(a, out);
            collect_leaves(b, out);
        }
    }
}

fn collect(node: &Node, area: Rect, out: &mut Vec<PaneInfo>) {
    match node {
        Node::Leaf(id) => out.push(PaneInfo {
            id: *id,
            rect: area,
        }),
        Node::Split { axis, ratio, a, b } => {
            let (ra, rb) = split_rect(area, *axis, *ratio);
            collect(a, ra, out);
            collect(b, rb, out);
        }
    }
}

/// Gap (in cells) between split children — configurable via Settings (Pane
/// Layout). Defaults: a one-column gap between left/right panes, none between
/// top/bottom. Stored as atomics so the recursive split math can read them
/// without threading config through every call.
use std::sync::atomic::{AtomicU16, Ordering};
static GAP_COL: AtomicU16 = AtomicU16::new(1);
static GAP_ROW: AtomicU16 = AtomicU16::new(0);

/// Set the inter-pane gaps (from `config.layout`). Clamped to 0..=1.
pub fn set_gaps(col: u16, row: u16) {
    GAP_COL.store(col.min(1), Ordering::Relaxed);
    GAP_ROW.store(row.min(1), Ordering::Relaxed);
}

fn split_rect(area: Rect, axis: Axis, ratio: f32) -> (Rect, Rect) {
    let gap_col = GAP_COL.load(Ordering::Relaxed);
    let gap_row = GAP_ROW.load(Ordering::Relaxed);
    match axis {
        Axis::Col => {
            let avail = area.width.saturating_sub(gap_col);
            let w1 = ((avail as f32) * ratio)
                .round()
                .clamp(1.0, (avail.saturating_sub(1)).max(1) as f32) as u16;
            let w2 = avail.saturating_sub(w1);
            (
                Rect::new(area.x, area.y, w1, area.height),
                Rect::new(area.x + w1 + gap_col, area.y, w2, area.height),
            )
        }
        Axis::Row => {
            let avail = area.height.saturating_sub(gap_row);
            let h1 = ((avail as f32) * ratio)
                .round()
                .clamp(1.0, (avail.saturating_sub(1)).max(1) as f32) as u16;
            let h2 = avail.saturating_sub(h1);
            (
                Rect::new(area.x, area.y, area.width, h1),
                Rect::new(area.x, area.y + h1 + gap_row, area.width, h2),
            )
        }
    }
}

fn split_at(node: &mut Node, target: PaneId, axis: Axis, new_id: PaneId) -> bool {
    match node {
        Node::Leaf(id) if *id == target => {
            let old = *id;
            *node = Node::Split {
                axis,
                ratio: 0.5,
                a: Box::new(Node::Leaf(old)),
                b: Box::new(Node::Leaf(new_id)),
            };
            true
        }
        Node::Leaf(_) => false,
        Node::Split { a, b, .. } => {
            split_at(a, target, axis, new_id) || split_at(b, target, axis, new_id)
        }
    }
}

fn remove_node(node: Node, id: PaneId) -> Option<Node> {
    match node {
        Node::Leaf(x) => {
            if x == id {
                None
            } else {
                Some(Node::Leaf(x))
            }
        }
        Node::Split { axis, ratio, a, b } => {
            let na = remove_node(*a, id);
            let nb = remove_node(*b, id);
            match (na, nb) {
                (Some(a), Some(b)) => Some(Node::Split {
                    axis,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                // One child removed → collapse to the survivor.
                (Some(x), None) | (None, Some(x)) => Some(x),
                (None, None) => None,
            }
        }
    }
}

fn first_leaf(node: &Node) -> PaneId {
    match node {
        Node::Leaf(id) => *id,
        Node::Split { a, .. } => first_leaf(a),
    }
}

fn node_to_tree(node: &Node) -> LayoutTree {
    match node {
        Node::Leaf(id) => LayoutTree::Leaf(id.0),
        Node::Split { axis, ratio, a, b } => LayoutTree::Split {
            axis: match axis {
                Axis::Col => 0,
                Axis::Row => 1,
            },
            ratio: *ratio,
            a: Box::new(node_to_tree(a)),
            b: Box::new(node_to_tree(b)),
        },
    }
}

fn build_node(tree: &LayoutTree, remap: &HashMap<u32, PaneId>) -> Option<Node> {
    match tree {
        LayoutTree::Leaf(raw) => remap.get(raw).map(|id| Node::Leaf(*id)),
        LayoutTree::Split { axis, ratio, a, b } => {
            let na = build_node(a, remap);
            let nb = build_node(b, remap);
            match (na, nb) {
                (Some(a), Some(b)) => Some(Node::Split {
                    axis: if *axis == 0 { Axis::Col } else { Axis::Row },
                    ratio: *ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (Some(x), None) | (None, Some(x)) => Some(x),
                (None, None) => None,
            }
        }
    }
}

fn center(r: Rect) -> (i32, i32) {
    (
        r.x as i32 + r.width as i32 / 2,
        r.y as i32 + r.height as i32 / 2,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_navigate() {
        let a = PaneId::alloc();
        let mut l = TileLayout::new(a);
        assert_eq!(l.len(), 1);

        let b = PaneId::alloc();
        l.split_focused(Axis::Col, b); // a | b
        assert_eq!(l.len(), 2);
        assert_eq!(l.focus, b);

        let area = Rect::new(0, 0, 80, 24);
        l.focus_dir(area, Dir::Left);
        assert_eq!(l.focus, a);
        l.focus_dir(area, Dir::Right);
        assert_eq!(l.focus, b);

        assert!(!l.remove(b)); // back to just `a`
        assert_eq!(l.len(), 1);
        assert_eq!(l.focus, a);
        assert!(l.remove(a)); // empty
    }
}
