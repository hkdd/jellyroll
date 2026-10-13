//! Spatial focus graph (spec 10.2). Every focusable node reports its rect.
//! Moving in a direction picks the nearest node in that direction,
//! preferring nodes aligned with the current one (in its "beam") and
//! weighting distance along the direction of travel over cross-axis
//! offset. Explicit overrides win over the search. There is deliberately no
//! per-row focus memory: moving always goes to what is spatially next
//! (D27).
//!
//! Focus is data here, one source of truth: widgets ask the graph whether
//! they are focused rather than keeping their own flags.

use std::collections::HashMap;

use jellyroll_render::Rect;

use crate::input::InputAction;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    pub fn from_action(a: InputAction) -> Option<Dir> {
        match a {
            InputAction::Up => Some(Dir::Up),
            InputAction::Down => Some(Dir::Down),
            InputAction::Left => Some(Dir::Left),
            InputAction::Right => Some(Dir::Right),
            _ => None,
        }
    }
}

pub type NodeId = u32;

/// Weight of the major (travel) axis distance against the minor one; the
/// same constant Android's focus finder uses, so a node straight ahead beats
/// a slightly closer one off to the side.
const MAJOR_WEIGHT: f32 = 13.0;

#[derive(Copy, Clone, Debug)]
struct Node {
    id: NodeId,
    rect: Rect,
}

#[derive(Default)]
pub struct FocusGraph {
    nodes: Vec<Node>,
    index: HashMap<NodeId, usize>,
    overrides: HashMap<(NodeId, Dir), NodeId>,
    focused: Option<NodeId>,
}

impl FocusGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the layout (and clears overrides). Focus survives if its
    /// node still exists; otherwise it moves to the first node.
    pub fn set_nodes(&mut self, nodes: impl IntoIterator<Item = (NodeId, Rect)>) {
        self.nodes.clear();
        self.index.clear();
        self.overrides.clear();
        for (id, rect) in nodes {
            self.index.insert(id, self.nodes.len());
            self.nodes.push(Node { id, rect });
        }
        if !self.focused.is_some_and(|id| self.index.contains_key(&id)) {
            self.focused = None;
            if let Some(first) = self.nodes.first().map(|n| n.id) {
                self.focus(first);
            }
        }
    }

    /// Explicit navigation edge, checked before the spatial search (e.g.
    /// wrapping at the end of a library grid row in M6).
    #[allow(dead_code)]
    pub fn set_override(&mut self, from: NodeId, dir: Dir, to: NodeId) {
        self.overrides.insert((from, dir), to);
    }

    pub fn focused(&self) -> Option<NodeId> {
        self.focused
    }

    pub fn is_focused(&self, id: NodeId) -> bool {
        self.focused == Some(id)
    }

    /// Focuses `id`; returns true if focus changed.
    pub fn focus(&mut self, id: NodeId) -> bool {
        if !self.index.contains_key(&id) || self.focused == Some(id) {
            return false;
        }
        self.focused = Some(id);
        true
    }

    /// Moves focus; returns the newly focused node, or None at an edge.
    pub fn navigate(&mut self, dir: Dir) -> Option<NodeId> {
        let next = self.neighbor(dir)?;
        self.focus(next).then_some(next)
    }

    /// The node focus would move to, without moving it.
    pub fn neighbor(&self, dir: Dir) -> Option<NodeId> {
        let cur = self.nodes[*self.index.get(&self.focused?)?];
        if let Some(&to) = self.overrides.get(&(cur.id, dir)) {
            return self.index.contains_key(&to).then_some(to);
        }
        let best = self
            .nodes
            .iter()
            .filter(|n| n.id != cur.id && is_candidate(&cur.rect, &n.rect, dir))
            .min_by(|a, b| {
                let ka = rank(&cur.rect, &a.rect, dir);
                let kb = rank(&cur.rect, &b.rect, dir);
                ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
            })?;
        Some(best.id)
    }
}

/// `dest` lies in direction `dir` from `src`: it extends further that way
/// and does not start behind `src`.
fn is_candidate(src: &Rect, dest: &Rect, dir: Dir) -> bool {
    match dir {
        Dir::Right => dest.x > src.x && dest.right() > src.right(),
        Dir::Left => dest.right() < src.right() && dest.x < src.x,
        Dir::Down => dest.y > src.y && dest.bottom() > src.bottom(),
        Dir::Up => dest.bottom() < src.bottom() && dest.y < src.y,
    }
}

/// Sort key: nodes in the beam (overlapping on the cross axis) first, then
/// by weighted distance.
fn rank(src: &Rect, dest: &Rect, dir: Dir) -> (u8, f32) {
    let horizontal = matches!(dir, Dir::Left | Dir::Right);
    let (s0, s1, d0, d1) = if horizontal {
        (src.y, src.bottom(), dest.y, dest.bottom())
    } else {
        (src.x, src.right(), dest.x, dest.right())
    };
    let in_beam = d0 < s1 && d1 > s0;
    let major = match dir {
        Dir::Right => dest.x - src.right(),
        Dir::Left => src.x - dest.right(),
        Dir::Down => dest.y - src.bottom(),
        Dir::Up => src.y - dest.bottom(),
    }
    .max(0.0);
    let minor = ((s0 + s1) - (d0 + d1)).abs() / 2.0;
    (u8::from(!in_beam), MAJOR_WEIGHT * major * major + minor * minor)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rows×cols grid of 100×150 cards; node id is row * 10 + col.
    fn grid(rows: u32, cols: u32) -> Vec<(NodeId, Rect)> {
        let mut v = Vec::new();
        for r in 0..rows {
            for c in 0..cols {
                let rect = Rect::new(c as f32 * 116.0, 100.0 + r as f32 * 200.0, 100.0, 150.0);
                v.push((r * 10 + c, rect));
            }
        }
        v
    }

    #[test]
    fn grid_navigation_and_edges() {
        let mut g = FocusGraph::new();
        g.set_nodes(grid(3, 4));
        assert_eq!(g.focused(), Some(0), "first node focused by default");
        assert_eq!(g.navigate(Dir::Left), None);
        assert_eq!(g.navigate(Dir::Up), None);
        assert_eq!(g.navigate(Dir::Right), Some(1));
        assert_eq!(g.navigate(Dir::Down), Some(11));
        assert_eq!(g.navigate(Dir::Down), Some(21));
        assert_eq!(g.navigate(Dir::Down), None);
        g.focus(23);
        assert_eq!(g.navigate(Dir::Right), None);
    }

    #[test]
    fn no_row_memory_moves_straight() {
        let mut g = FocusGraph::new();
        g.set_nodes(grid(2, 6));
        g.focus(4);
        assert_eq!(g.navigate(Dir::Down), Some(14));
        assert_eq!(g.navigate(Dir::Left), Some(13));
        assert_eq!(g.navigate(Dir::Left), Some(12));
        // Up goes straight up, not back to where row 0 was left.
        assert_eq!(g.navigate(Dir::Up), Some(2));
    }

    #[test]
    fn aligned_beats_closer_diagonal() {
        let mut g = FocusGraph::new();
        g.set_nodes([
            (1, Rect::new(0.0, 0.0, 100.0, 40.0)),
            // Slightly below-right and close, but not in the beam.
            (2, Rect::new(110.0, 60.0, 100.0, 40.0)),
            // Straight right, further away.
            (3, Rect::new(400.0, 0.0, 100.0, 40.0)),
        ]);
        assert_eq!(g.neighbor(Dir::Right), Some(3));
    }

    #[test]
    fn narrow_to_wide_and_back() {
        // A nav bar of small pills above wide cards: down picks the card
        // under the pill; up picks the pill above the card.
        let mut g = FocusGraph::new();
        let mut nodes = vec![
            (100, Rect::new(0.0, 0.0, 80.0, 30.0)),
            (101, Rect::new(90.0, 0.0, 80.0, 30.0)),
            (102, Rect::new(180.0, 0.0, 80.0, 30.0)),
        ];
        nodes.extend(grid(1, 3));
        g.set_nodes(nodes);
        g.focus(102);
        assert_eq!(g.navigate(Dir::Down), Some(1), "card under x=180..260");
        g.focus(2);
        assert_eq!(g.navigate(Dir::Up), Some(102), "nearest pill above, by distance");
    }

    #[test]
    fn overrides_win() {
        let mut g = FocusGraph::new();
        g.set_nodes(grid(1, 4));
        g.set_override(3, Dir::Right, 0);
        g.focus(3);
        assert_eq!(g.navigate(Dir::Right), Some(0), "wraps by override");
    }

    #[test]
    fn relayout_keeps_focus_and_drops_missing() {
        let mut g = FocusGraph::new();
        g.set_nodes(grid(2, 4));
        g.focus(13);
        g.set_nodes(grid(2, 4));
        assert_eq!(g.focused(), Some(13), "same ids keep focus");
        g.set_nodes(grid(1, 4));
        assert_eq!(g.focused(), Some(0), "focused node removed: first node");
    }
}
