//! One project's pane arrangement: a tree of splits over window ids.
//!
//! A preset seeds the tree, and while the tree still has the shape the
//! preset would give it, new windows keep being placed the preset's way.
//! Once a header has been dropped somewhere the tree is the user's, and
//! from then on it only changes where they say: a drop on a pane's edge
//! splits that pane, a drop on its middle swaps the two, a new window is
//! appended at the root.
//!
//! Divider positions live here too, in pixels, so a rebuild for one drop
//! does not reset every divider the user had dragged.

use crate::presets::Preset;
use gtk::prelude::*;
use taix_core::AgentId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Leaf(AgentId),
    Split {
        vertical: bool,
        children: Vec<Node>,
        /// One per divider: pixels, or 0 for "let GTK decide".
        positions: Vec<i32>,
    },
    Tabs {
        ids: Vec<AgentId>,
        active: usize,
    },
}

/// Where on a pane a header was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
    Centre,
    Tab,
}

impl Side {
    /// Where a dropped header lands: its own header strip makes a tab, the
    /// outer quarter of the body picks an edge, the middle swaps. The zones
    /// are measured inside the body, not the whole card - a card is mostly
    /// body, and a quarter of the card would put "top" under the strip.
    pub fn at(x: f64, y: f64, w: f64, h: f64, head: f64) -> Side {
        if w <= 0.0 || h <= 0.0 {
            return Side::Centre;
        }
        if y < head {
            return Side::Tab;
        }
        let body = (h - head).max(1.0);
        let down = (y - head).clamp(0.0, body);
        let dx = x.min(w - x) / w;
        let dy = down.min(body - down) / body;
        if dx >= 0.25 && dy >= 0.25 {
            Side::Centre
        } else if dx <= dy {
            if x < w / 2.0 { Side::Left } else { Side::Right }
        } else if down < body / 2.0 {
            Side::Top
        } else {
            Side::Bottom
        }
    }
}

fn split(vertical: bool, children: Vec<Node>) -> Node {
    let positions = vec![0; children.len().saturating_sub(1)];
    Node::Split {
        vertical,
        children,
        positions,
    }
}

impl Node {
    pub fn from_preset(ids: &[AgentId], preset: Preset) -> Option<Node> {
        fn balanced(ids: &[AgentId], vertical: bool) -> Node {
            match ids.len() {
                1 => Node::Leaf(ids[0]),
                n => {
                    let (left, right) = ids.split_at(n.div_ceil(2));
                    split(
                        vertical,
                        vec![balanced(left, !vertical), balanced(right, !vertical)],
                    )
                }
            }
        }
        let leaves = |ids: &[AgentId]| ids.iter().map(|id| Node::Leaf(*id)).collect::<Vec<_>>();
        match ids.len() {
            0 => None,
            1 => Some(Node::Leaf(ids[0])),
            _ => Some(match preset {
                Preset::Balanced => balanced(ids, false),
                Preset::Columns => split(false, leaves(ids)),
                Preset::Rows => split(true, leaves(ids)),
                Preset::MainLeft => split(
                    false,
                    vec![Node::Leaf(ids[0]), split(true, leaves(&ids[1..]))],
                ),
                Preset::MainTop => split(
                    true,
                    vec![Node::Leaf(ids[0]), split(false, leaves(&ids[1..]))],
                ),
            }),
        }
    }

    pub fn leaves(&self) -> Vec<AgentId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<AgentId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { children, .. } => children.iter().for_each(|c| c.collect(out)),
            Node::Tabs { ids, .. } => out.extend(ids),
        }
    }

    /// Every window that is in a tab group: those cards hide their own
    /// header, because the strip already names them.
    pub fn tabbed(&self) -> Vec<AgentId> {
        match self {
            Node::Leaf(_) => Vec::new(),
            Node::Tabs { ids, .. } => ids.clone(),
            Node::Split { children, .. } => children.iter().flat_map(Node::tabbed).collect(),
        }
    }
    /// Shape only: what `encode` writes, so positions are not compared.
    fn same_shape(&self, other: &Node) -> bool {
        self.encode() == other.encode()
    }

    /// Make the tree show exactly `ids`, in a way that respects what the user
    /// has done to it: a tree still in its preset shape is regenerated, so a
    /// balanced grid stays balanced; a customised one loses only the windows
    /// that went and gains the new ones at its root.
    pub fn sync(tree: Option<Node>, ids: &[AgentId], preset: Preset) -> Option<Node> {
        let Some(tree) = tree else {
            return Node::from_preset(ids, preset);
        };
        let had = tree.leaves();
        let pristine = Node::from_preset(&had, preset).is_some_and(|p| p.same_shape(&tree));
        if pristine {
            return Node::from_preset(ids, preset);
        }
        let mut tree = Some(tree);
        for gone in had.iter().filter(|id| !ids.contains(id)) {
            tree = tree.and_then(|t| t.without(*gone));
        }
        for new in ids.iter().filter(|id| !had.contains(id)) {
            tree = Some(match tree {
                None => Node::Leaf(*new),
                Some(Node::Split {
                    vertical,
                    mut children,
                    mut positions,
                }) => {
                    children.push(Node::Leaf(*new));
                    positions.push(0);
                    Node::Split {
                        vertical,
                        children,
                        positions,
                    }
                }
                Some(leaf) => split(
                    matches!(preset, Preset::Rows | Preset::MainTop),
                    vec![leaf, Node::Leaf(*new)],
                ),
            });
        }
        tree
    }

    /// The tree with one window taken out; splits left with one child fold.
    pub fn without(self, id: AgentId) -> Option<Node> {
        match self {
            Node::Leaf(x) => (x != id).then_some(Node::Leaf(x)),
            Node::Tabs { mut ids, active } => {
                if let Some(i) = ids.iter().position(|x| *x == id) {
                    ids.remove(i);
                    match ids.len() {
                        0 => None,
                        1 => Some(Node::Leaf(ids[0])),
                        n => Some(Node::Tabs {
                            ids,
                            active: active.min(n - 1),
                        }),
                    }
                } else {
                    Some(Node::Tabs { ids, active })
                }
            }
            Node::Split {
                vertical,
                children,
                mut positions,
            } => {
                let mut kept = Vec::with_capacity(children.len());
                for (i, child) in children.into_iter().enumerate() {
                    match child.without(id) {
                        Some(c) => kept.push(c),
                        None if !positions.is_empty() => {
                            positions.remove(i.min(positions.len() - 1));
                        }
                        None => {}
                    }
                }
                match kept.len() {
                    0 => None,
                    1 => kept.pop(),
                    _ => Some(Node::Split {
                        vertical,
                        children: kept,
                        positions,
                    }),
                }
            }
        }
    }
    pub fn swap(&mut self, a: AgentId, b: AgentId) {
        match self {
            Node::Leaf(x) if *x == a => *x = b,
            Node::Leaf(x) if *x == b => *x = a,
            Node::Leaf(_) => {}
            Node::Tabs { ids, .. } => {
                for x in ids.iter_mut() {
                    if *x == a {
                        *x = b;
                    } else if *x == b {
                        *x = a;
                    }
                }
            }
            Node::Split { children, .. } => children.iter_mut().for_each(|c| c.swap(a, b)),
        }
    }
    /// Pull `from` out of the tree and put it in `onto`'s tab group. Creates
    /// the group if `onto` is a plain leaf; makes `from` the active tab.
    pub fn tab(self, from: AgentId, onto: AgentId) -> Node {
        let Some(tree) = self.without(from) else {
            return Node::Leaf(from);
        };
        tree.insert_tab(from, onto)
    }

    fn insert_tab(self, from: AgentId, onto: AgentId) -> Node {
        match self {
            Node::Leaf(x) if x == onto => Node::Tabs {
                ids: vec![onto, from],
                active: 1,
            },
            Node::Tabs { mut ids, .. } if ids.contains(&onto) => {
                ids.push(from);
                Node::Tabs {
                    active: ids.len() - 1,
                    ids,
                }
            }
            Node::Leaf(x) => Node::Leaf(x),
            Node::Tabs { ids, active } => Node::Tabs { ids, active },
            Node::Split {
                vertical,
                children,
                positions,
            } => Node::Split {
                vertical,
                children: children
                    .into_iter()
                    .map(|c| c.insert_tab(from, onto))
                    .collect(),
                positions,
            },
        }
    }

    /// Make `id` the active tab of its group. Returns whether anything changed.
    pub fn activate(&mut self, id: AgentId) -> bool {
        match self {
            Node::Tabs { ids, active } => {
                if let Some(i) = ids.iter().position(|x| *x == id)
                    && *active != i
                {
                    *active = i;
                    return true;
                }
                false
            }
            Node::Split { children, .. } => children.iter_mut().any(|c| c.activate(id)),
            Node::Leaf(_) => false,
        }
    }

    /// Put `from` beside `onto` on `side`. A split already running that way
    /// takes it as a sibling, so three drops to the right make three columns
    /// rather than a staircase of nested splits.
    pub fn dock(self, from: AgentId, onto: AgentId, side: Side) -> Node {
        let Some(tree) = self.without(from) else {
            return Node::Leaf(from);
        };
        let vertical = matches!(side, Side::Top | Side::Bottom);
        let before = matches!(side, Side::Left | Side::Top);
        tree.place(from, onto, vertical, before)
    }

    fn place(self, from: AgentId, onto: AgentId, vertical: bool, before: bool) -> Node {
        match self {
            Node::Leaf(x) if x == onto => {
                let pair = if before {
                    vec![Node::Leaf(from), Node::Leaf(onto)]
                } else {
                    vec![Node::Leaf(onto), Node::Leaf(from)]
                };
                split(vertical, pair)
            }
            Node::Leaf(x) => Node::Leaf(x),
            Node::Tabs { .. } => self,
            Node::Split {
                vertical: v,
                mut children,
                positions,
            } => {
                let slot = children.iter().position(|c| *c == Node::Leaf(onto));
                match slot {
                    Some(i) if v == vertical => {
                        children.insert(if before { i } else { i + 1 }, Node::Leaf(from));
                        split(v, children)
                    }
                    _ => Node::Split {
                        vertical: v,
                        children: children
                            .into_iter()
                            .map(|c| c.place(from, onto, vertical, before))
                            .collect(),
                        positions,
                    },
                }
            }
        }
    }

    /// `t[2,4:1]` (tab group: ids 2 and 4, active index 1) or `h[3,v[4,5]]`.
    pub fn encode(&self) -> String {
        match self {
            Node::Leaf(id) => id.to_string(),
            Node::Split {
                vertical, children, ..
            } => {
                let inner: Vec<String> = children.iter().map(Node::encode).collect();
                format!("{}[{}]", if *vertical { 'v' } else { 'h' }, inner.join(","))
            }
            Node::Tabs { ids, active } => {
                format!(
                    "t[{}:{}]",
                    ids.iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                    active
                )
            }
        }
    }

    pub fn decode(text: &str) -> Option<Node> {
        fn node(s: &str) -> Option<(Node, &str)> {
            let s = s.trim_start_matches(',');
            let first = s.chars().next()?;
            if first == 'h' || first == 'v' {
                let mut rest = s.get(1..)?.strip_prefix('[')?;
                let mut children = Vec::new();
                while !rest.starts_with(']') {
                    let (child, after) = node(rest)?;
                    children.push(child);
                    rest = after;
                }
                if children.len() < 2 {
                    return None;
                }
                Some((split(first == 'v', children), rest.get(1..)?))
            } else if first == 't' {
                let rest = s.get(1..)?.strip_prefix('[')?;
                let end = rest.find(']')?;
                let inner = &rest[..end];
                let (ids_part, active_part) = inner.rsplit_once(':')?;
                let ids: Vec<AgentId> =
                    ids_part.split(',').filter_map(|s| s.parse().ok()).collect();
                let active: usize = active_part.parse().ok()?;
                if ids.len() < 2 || active >= ids.len() {
                    return None;
                }
                Some((Node::Tabs { ids, active }, rest.get(end + 1..)?))
            } else {
                let end = s.find([',', ']', ':']).unwrap_or(s.len());
                Some((Node::Leaf(s[..end].parse().ok()?), &s[end..]))
            }
        }
        let (tree, rest) = node(text.trim())?;
        rest.is_empty().then_some(tree)
    }

    /// The widget tree: a `GtkPaned` per divider, a card per leaf. A split of
    /// n children is a chain of n-1 paneds, each holding one child and the
    /// The widget tree: a `GtkPaned` per divider, a card per leaf, a strip
    /// plus card per tab group.
    pub fn render(
        &self,
        card: &dyn Fn(AgentId) -> Option<gtk::Widget>,
        strip: &dyn Fn(&[AgentId], usize) -> gtk::Widget,
    ) -> gtk::Widget {
        match self {
            Node::Leaf(id) => {
                card(*id).unwrap_or_else(|| gtk::Box::new(gtk::Orientation::Vertical, 0).upcast())
            }
            Node::Tabs { ids, active } => {
                let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
                container.append(&strip(ids, *active));
                if let Some(w) = card(ids[*active]) {
                    container.append(&w);
                }
                container.upcast()
            }
            Node::Split {
                vertical,
                children,
                positions,
            } => {
                let mut acc = children[children.len() - 1].render(card, strip);
                for (i, child) in children.iter().enumerate().rev().skip(1) {
                    let paned = gtk::Paned::builder()
                        .orientation(if *vertical {
                            gtk::Orientation::Vertical
                        } else {
                            gtk::Orientation::Horizontal
                        })
                        .start_child(&child.render(card, strip))
                        .end_child(&acc)
                        .resize_start_child(true)
                        .resize_end_child(true)
                        .shrink_start_child(false)
                        .shrink_end_child(false)
                        .build();
                    if positions[i] > 0 {
                        paned.set_position(positions[i]);
                    }
                    acc = paned.upcast();
                }
                acc
            }
        }
    }

    /// Read the dividers the user dragged back out of a widget tree that
    /// `render` built from this same shape. Untouched dividers stay 0, so
    /// they keep following the window rather than freezing where they were.
    pub fn harvest(&mut self, widget: &gtk::Widget) {
        let Node::Split {
            children,
            positions,
            ..
        } = self
        else {
            return;
        };
        let mut at = widget.clone();
        let last = children.len() - 1;
        for (i, child) in children.iter_mut().enumerate() {
            if i == last {
                child.harvest(&at);
                return;
            }
            let Some(paned) = at.downcast_ref::<gtk::Paned>().cloned() else {
                return;
            };
            positions[i] = if paned.property::<bool>("position-set") {
                paned.position()
            } else {
                0
            };
            if let Some(start) = paned.start_child() {
                child.harvest(&start);
            }
            let Some(end) = paned.end_child() else { return };
            at = end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: AgentId) -> Node {
        Node::Leaf(id)
    }

    #[test]
    fn presets_seed_the_expected_shapes() {
        let ids = [1, 2, 3, 4];
        let enc = |p| Node::from_preset(&ids, p).unwrap().encode();
        assert_eq!(enc(Preset::Balanced), "h[v[1,2],v[3,4]]");
        assert_eq!(enc(Preset::Columns), "h[1,2,3,4]");
        assert_eq!(enc(Preset::Rows), "v[1,2,3,4]");
        assert_eq!(enc(Preset::MainLeft), "h[1,v[2,3,4]]");
        assert_eq!(enc(Preset::MainTop), "v[1,h[2,3,4]]");
        assert_eq!(Node::from_preset(&[7], Preset::Rows), Some(leaf(7)));
        assert_eq!(Node::from_preset(&[], Preset::Rows), None);
    }

    #[test]
    fn encoding_round_trips_and_rejects_garbage() {
        for text in ["3", "h[1,2]", "v[1,h[2,3],4]", "h[v[1,2],v[3,4]]"] {
            assert_eq!(
                Node::decode(text).map(|n| n.encode()).as_deref(),
                Some(text)
            );
        }
        for bad in ["", "h[1]", "h[1,2", "x[1,2]", "h[a,b]", "1,2"] {
            assert_eq!(Node::decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn edge_drops_split_and_centre_swaps() {
        let tree = Node::from_preset(&[1, 2, 3], Preset::Columns).unwrap();
        // Below 2: 1 | (2 over 3)
        assert_eq!(
            tree.clone().dock(3, 2, Side::Bottom).encode(),
            "h[1,v[2,3]]"
        );
        // Left of 1 in a row that already runs horizontally: a sibling.
        assert_eq!(tree.clone().dock(3, 1, Side::Left).encode(), "h[3,1,2]");
        let mut swapped = tree.clone();
        swapped.swap(1, 3);
        assert_eq!(swapped.encode(), "h[3,2,1]");
        // Docking the only other window onto a leaf root.
        assert_eq!(leaf(1).dock(2, 1, Side::Top).encode(), "v[2,1]");
        // Pulling a window out of a nested split folds the split it leaves.
        let nested = Node::decode("h[1,v[2,3]]").unwrap();
        assert_eq!(nested.dock(3, 1, Side::Right).encode(), "h[1,3,2]");
    }

    #[test]
    fn sync_regenerates_pristine_trees_and_patches_custom_ones() {
        let grid = Node::from_preset(&[1, 2, 3], Preset::Balanced);
        assert_eq!(
            Node::sync(grid, &[1, 2, 3, 4], Preset::Balanced)
                .unwrap()
                .encode(),
            "h[v[1,2],v[3,4]]"
        );
        let custom = Node::decode("h[1,v[2,3]]");
        assert_eq!(
            Node::sync(custom.clone(), &[1, 3, 4], Preset::Balanced)
                .unwrap()
                .encode(),
            "h[1,3,4]"
        );
        assert_eq!(Node::sync(custom, &[], Preset::Balanced), None);
        assert_eq!(
            Node::sync(Some(leaf(1)), &[1, 2], Preset::Rows)
                .unwrap()
                .encode(),
            "v[1,2]"
        );
    }

    #[test]
    fn without_keeps_the_dividers_of_the_survivors() {
        let tree = Node::Split {
            vertical: false,
            children: vec![leaf(1), leaf(2), leaf(3)],
            positions: vec![100, 200],
        };
        match tree.clone().without(1).unwrap() {
            Node::Split { positions, .. } => assert_eq!(positions, vec![200]),
            other => panic!("{other:?}"),
        }
        match tree.without(3).unwrap() {
            Node::Split { positions, .. } => assert_eq!(positions, vec![100]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn tab_creates_and_extends_groups() {
        // Tabbing onto a leaf creates a group with from active.
        let tree = Node::from_preset(&[1, 2, 3], Preset::Columns).unwrap();
        assert_eq!(tree.clone().tab(3, 1).encode(), "h[t[1,3:1],2]");
        // Tabbing into an existing group appends.
        let tabbed = tree.tab(3, 1);
        assert_eq!(tabbed.tab(2, 1).encode(), "t[1,3,2:2]");
    }

    #[test]
    fn removing_last_but_one_tab_folds_to_leaf() {
        let tabs = Node::Tabs {
            ids: vec![1, 2],
            active: 0,
        };
        assert_eq!(tabs.without(1), Some(leaf(2)));
        assert_eq!(leaf(1).without(1), None);
    }

    #[test]
    fn tab_encode_decode_round_trips() {
        for text in ["t[2,4:1]", "t[1,2,3:0]", "h[t[1,2:1],3]"] {
            assert_eq!(
                Node::decode(text).map(|n| n.encode()).as_deref(),
                Some(text)
            );
        }
        // Active out of range rejected.
        assert_eq!(Node::decode("t[1,2:2]"), None);
        // Single-element tab rejected.
        assert_eq!(Node::decode("t[1:0]"), None);
    }

    #[test]
    fn sides_come_from_the_nearest_edge() {
        let head = 30.0;
        assert_eq!(Side::at(50.0, 50.0, 100.0, 100.0, head), Side::Centre);
        assert_eq!(Side::at(50.0, 10.0, 100.0, 100.0, head), Side::Tab);
        assert_eq!(Side::at(5.0, 50.0, 100.0, 100.0, head), Side::Left);
        assert_eq!(Side::at(95.0, 50.0, 100.0, 100.0, head), Side::Right);
        assert_eq!(Side::at(50.0, 35.0, 100.0, 100.0, head), Side::Top);
        assert_eq!(Side::at(50.0, 95.0, 100.0, 100.0, head), Side::Bottom);
        // In a corner the nearer edge wins.
        assert_eq!(Side::at(10.0, 50.0, 100.0, 100.0, head), Side::Left);
        assert_eq!(Side::at(50.0, 35.0, 100.0, 100.0, head), Side::Top);
    }
}
