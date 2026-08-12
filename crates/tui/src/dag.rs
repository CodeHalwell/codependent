//! ASCII DAG layout for the workflow pane (rubric 5).
//!
//! The workflow view used to be a *topological list*: correct order, no edges —
//! so "what is this node waiting on" could only be read off the detail rail, one
//! node at a time. This module turns the same ordered list into **layered lanes**
//! with box-drawing connectors, the way `git log --graph` renders history: each
//! node keeps its own row (so selection, scrolling, and hit regions are
//! untouched), and a lane prefix to its left shows the edges.
//!
//! It is deliberately a pure function over `(id, depends_on)` pairs — no
//! ratatui, no theme, no state — so the layout is unit-testable on its own and
//! the renderer stays a projection of it.
//!
//! **Degradation** is explicit, not accidental: [`lay_out`] reports the lane
//! width it needs, and a caller with too little room (or a graph with no edges at
//! all) simply renders the plain list it rendered before — the connectors are an
//! upgrade to the same rows, never a replacement view that could go missing.

/// One node's edge input: its id and the ids it depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagNode {
    /// The node id (unique within the graph passed to [`lay_out`]).
    pub id: String,
    /// The ids this node depends on. Unknown ids (a dependency outside this
    /// graph) are ignored rather than fabricating a lane for them.
    pub depends_on: Vec<String>,
}

/// The lane art for one node's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagRow {
    /// The lane this node occupies.
    pub lane: usize,
    /// The **connector** line drawn ABOVE the node's row, joining it to the
    /// dependencies that live in other lanes. Empty when the node's every
    /// dependency already sits in its own lane (nothing to join) — the caller
    /// then keeps whatever it would otherwise put on that line.
    pub connector: String,
    /// The node's own line: `●` at [`lane`](Self::lane), `│` for every other lane
    /// with an edge passing through this row.
    pub node: String,
    /// The continuation line drawn BELOW the node's row: `│` for every lane still
    /// carrying an edge (including this node's own, when something depends on it).
    pub trail: String,
}

/// The laid-out graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagLayout {
    /// One entry per input node, in the order given.
    pub rows: Vec<DagRow>,
    /// The number of lanes used — the display width of every prefix string is
    /// exactly this many characters.
    pub lanes: usize,
    /// Whether the graph has any edge at all. `false` means every node is a root,
    /// so lane art would be a column of `●` and the caller should render the
    /// plain list instead.
    pub has_edges: bool,
}

/// The widest graph the lane renderer draws. A workflow broad enough to need
/// more lanes than this would push the node ids off a narrow pane, so the caller
/// falls back to the plain list — bounded degradation rather than an unreadable
/// smear.
pub const MAX_LANES: usize = 6;

/// Lay out `nodes` (already in topological order) into lanes.
///
/// The assignment is the one that keeps a chain in a straight line: a node
/// inherits the lane of a dependency whose LAST dependent it is, so a linear
/// pipeline renders as a single column and only a genuine fan-out opens a second
/// lane. Forward or unknown dependency ids are ignored — this draws the graph it
/// was given, never invents one.
#[must_use]
pub fn lay_out(nodes: &[DagNode]) -> DagLayout {
    let row_of = |id: &str| nodes.iter().position(|node| node.id == id);

    // How many later nodes still depend on each row — a lane stays open until
    // this reaches zero.
    let mut pending: Vec<usize> = vec![0; nodes.len()];
    let mut has_edges = false;
    for (index, node) in nodes.iter().enumerate() {
        for dep in &node.depends_on {
            if let Some(dep_row) = row_of(dep) {
                // A dependency must come EARLIER: the input is topologically
                // ordered, and honoring a forward edge would mean drawing a lane
                // upward into a row already rendered.
                if dep_row < index {
                    pending[dep_row] += 1;
                    has_edges = true;
                }
            }
        }
    }

    // lanes[l] = the row whose outgoing edges currently occupy lane `l`.
    let mut lanes: Vec<Option<usize>> = Vec::new();
    let mut rows: Vec<DagRow> = Vec::with_capacity(nodes.len());
    // Provisional rows; the prefixes are padded to the final lane count at the
    // end, so every row lines up even though lanes open as the graph fans out.
    let mut raw: Vec<(usize, Vec<char>, Vec<char>, Vec<char>)> = Vec::with_capacity(nodes.len());

    for (index, node) in nodes.iter().enumerate() {
        let mut dep_lanes: Vec<usize> = node
            .depends_on
            .iter()
            .filter_map(|dep| row_of(dep))
            .filter(|dep_row| *dep_row < index)
            .filter_map(|dep_row| lanes.iter().position(|held| *held == Some(dep_row)))
            .collect();
        dep_lanes.sort_unstable();
        dep_lanes.dedup();

        // Retire the lanes of dependencies this node is the last dependent of.
        for &lane in &dep_lanes {
            if let Some(dep_row) = lanes[lane] {
                pending[dep_row] -= 1;
                if pending[dep_row] == 0 {
                    lanes[lane] = None;
                }
            }
        }

        // Prefer a freed dependency lane, so a chain stays in one column.
        let own_lane = dep_lanes
            .iter()
            .copied()
            .find(|lane| lanes[*lane].is_none())
            .or_else(|| lanes.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                lanes.push(None);
                lanes.len() - 1
            });

        // The connector row: horizontals spanning this node's lane and every
        // dependency lane it is joining from, `┴` where an edge rises out of a
        // dependency's lane and `┬` where it drops into this node's.
        let joining: Vec<usize> = dep_lanes
            .iter()
            .copied()
            .filter(|lane| *lane != own_lane)
            .collect();
        let connector = if joining.is_empty() {
            Vec::new()
        } else {
            let lo = joining
                .iter()
                .copied()
                .min()
                .unwrap_or(own_lane)
                .min(own_lane);
            let hi = joining
                .iter()
                .copied()
                .max()
                .unwrap_or(own_lane)
                .max(own_lane);
            (0..lanes.len().max(own_lane + 1))
                .map(|lane| {
                    if lane == own_lane {
                        '\u{252c}' // ┬ — the edge drops into this node
                    } else if joining.contains(&lane) {
                        '\u{2534}' // ┴ — the edge rises from a dependency
                    } else if lane > lo && lane < hi {
                        // A lane crossed by the horizontal: `┼` when it is itself
                        // still carrying an edge, plain `─` otherwise.
                        if lanes[lane].is_some() {
                            '\u{253c}' // ┼
                        } else {
                            '\u{2500}' // ─
                        }
                    } else if lanes.get(lane).copied().flatten().is_some() {
                        '\u{2502}' // │
                    } else {
                        ' '
                    }
                })
                .collect()
        };

        let node_line: Vec<char> = (0..lanes.len().max(own_lane + 1))
            .map(|lane| {
                if lane == own_lane {
                    '\u{25cf}' // ●
                } else if lanes.get(lane).copied().flatten().is_some() {
                    '\u{2502}' // │
                } else {
                    ' '
                }
            })
            .collect();

        // Claim the lane only if something later depends on this node.
        if own_lane >= lanes.len() {
            lanes.resize(own_lane + 1, None);
        }
        lanes[own_lane] = (pending[index] > 0).then_some(index);

        let trail: Vec<char> = (0..lanes.len())
            .map(|lane| {
                if lanes[lane].is_some() {
                    '\u{2502}' // │
                } else {
                    ' '
                }
            })
            .collect();

        raw.push((own_lane, connector, node_line, trail));
    }

    let width = lanes
        .len()
        .max(raw.iter().map(|r| r.0 + 1).max().unwrap_or(0));
    for (lane, connector, node_line, trail) in raw {
        let pad = |mut chars: Vec<char>| -> String {
            if chars.is_empty() {
                return String::new();
            }
            chars.resize(width, ' ');
            chars.into_iter().collect()
        };
        rows.push(DagRow {
            lane,
            connector: pad(connector),
            node: pad(node_line),
            trail: pad(trail),
        });
    }

    DagLayout {
        rows,
        lanes: width,
        has_edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes(spec: &[(&str, &[&str])]) -> Vec<DagNode> {
        spec.iter()
            .map(|(id, deps)| DagNode {
                id: (*id).to_string(),
                depends_on: deps.iter().map(|d| (*d).to_string()).collect(),
            })
            .collect()
    }

    #[test]
    fn a_linear_chain_stays_in_one_lane() {
        // The common case (`/fix-ci`: investigate → patch → verify → review):
        // every node is its predecessor's only dependent, so the graph is one
        // column and no connector rows are drawn.
        let layout = lay_out(&nodes(&[
            ("investigate", &[]),
            ("patch", &["investigate"]),
            ("verify", &["patch"]),
        ]));
        assert!(layout.has_edges);
        assert_eq!(layout.lanes, 1);
        for row in &layout.rows {
            assert_eq!(row.lane, 0);
            assert_eq!(row.node, "\u{25cf}");
            assert_eq!(row.connector, "", "a straight chain needs no connector");
        }
        // The last node has no dependents, so its lane closes.
        assert_eq!(layout.rows[2].trail, " ");
        assert_eq!(layout.rows[0].trail, "\u{2502}");
    }

    #[test]
    fn a_fan_out_opens_a_second_lane_and_a_fan_in_joins_it() {
        //        root
        //       /    \
        //     left   right
        //       \    /
        //        merge
        let layout = lay_out(&nodes(&[
            ("root", &[]),
            ("left", &["root"]),
            ("right", &["root"]),
            ("merge", &["left", "right"]),
        ]));
        assert_eq!(layout.lanes, 2);
        // `root` keeps lane 0 while it still has an unplaced dependent, so `left`
        // branches into a new lane and `right` — root's LAST dependent — inherits
        // lane 0. That is what keeps the trunk straight instead of shuffling the
        // main line sideways at every fan-out.
        assert_eq!(layout.rows[1].lane, 1);
        assert_eq!(layout.rows[1].connector, "\u{2534}\u{252c}");
        assert_eq!(layout.rows[2].lane, 0);
        // While `right` is drawn, `left`'s edge to `merge` passes through lane 1.
        assert_eq!(layout.rows[2].node, "\u{25cf}\u{2502}");
        // The fan-in draws a connector joining lane 1 down into lane 0.
        assert_eq!(layout.rows[3].lane, 0);
        assert_eq!(layout.rows[3].connector, "\u{252c}\u{2534}");
        assert_eq!(layout.rows[3].node, "\u{25cf} ");
        // Nothing depends on `merge`, so every lane closes.
        assert_eq!(layout.rows[3].trail, "  ");
    }

    #[test]
    fn an_edgeless_graph_reports_no_edges_so_the_caller_keeps_the_plain_list() {
        let layout = lay_out(&nodes(&[("a", &[]), ("b", &[])]));
        assert!(!layout.has_edges);
        // Independent roots still lay out (lane 0 is reused once `a` closes).
        assert_eq!(layout.rows[1].lane, 0);
    }

    #[test]
    fn unknown_and_forward_dependencies_are_ignored_not_invented() {
        // `a` names a dependency that is not in this graph, and `b` names one
        // that comes LATER (which a topological order rules out). Drawing either
        // would mean inventing a lane or an upward edge.
        let layout = lay_out(&nodes(&[("a", &["ghost"]), ("b", &["c"]), ("c", &[])]));
        assert!(!layout.has_edges);
        assert!(layout.rows.iter().all(|row| row.connector.is_empty()));
    }

    #[test]
    fn every_prefix_in_a_layout_has_the_same_display_width() {
        // The renderer puts these prefixes in front of node text on consecutive
        // lines; a ragged width would visibly shear the list.
        let layout = lay_out(&nodes(&[
            ("root", &[]),
            ("a", &["root"]),
            ("b", &["root"]),
            ("c", &["root"]),
            ("end", &["a", "b", "c"]),
        ]));
        assert_eq!(layout.lanes, 3);
        for row in &layout.rows {
            assert_eq!(row.node.chars().count(), layout.lanes);
            assert_eq!(row.trail.chars().count(), layout.lanes);
            assert!(row.connector.is_empty() || row.connector.chars().count() == layout.lanes);
        }
        // The three-way fan-in joins both outer lanes into lane 0.
        assert_eq!(layout.rows[4].connector, "\u{252c}\u{2534}\u{2534}");
    }

    #[test]
    fn a_crossed_lane_that_still_carries_an_edge_is_drawn_as_a_junction() {
        // A three-way fan-out: `b` branches to lane 2 while `a` still holds lane
        // 1, so `b`'s connector must CROSS lane 1 — and lane 1 is itself carrying
        // an open edge, so the crossing is `┼`, not a `─` that would erase it.
        let layout = lay_out(&nodes(&[
            ("root", &[]),
            ("a", &["root"]),
            ("b", &["root"]),
            ("c", &["root"]),
            ("end", &["a", "c"]),
            ("last", &["b"]),
        ]));
        assert_eq!(
            layout.rows[2].connector, "\u{2534}\u{253c}\u{252c}",
            "the join must cross an open lane as ┼"
        );
        // And a lane OUTSIDE the join's span is left alone as a plain `│`: `end`
        // joins lanes 0 and 1 while `b` still holds lane 2.
        assert_eq!(layout.rows[4].connector, "\u{252c}\u{2534}\u{2502}");
    }
}
