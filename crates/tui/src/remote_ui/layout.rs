use codypendent_protocol::remote_ui::{UiDimension, UiEdges, UiLayout, UiNode};
use ratatui::layout::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    Horizontal,
    Vertical,
}

pub(crate) fn inset(area: Rect, edges: UiEdges) -> Rect {
    let left = cells(edges.left);
    let right = cells(edges.right);
    let top = cells(edges.top);
    let bottom = cells(edges.bottom);
    let x = area.x.saturating_add(left.min(area.width));
    let y = area.y.saturating_add(top.min(area.height));
    let width = area.width.saturating_sub(left).saturating_sub(right);
    let height = area.height.saturating_sub(top).saturating_sub(bottom);
    Rect::new(x, y, width, height)
}

pub(crate) fn margin(area: Rect, layout: Option<&UiLayout>) -> Rect {
    layout
        .and_then(|layout| layout.margin)
        .map_or(area, |edges| inset(area, edges))
}

pub(crate) fn content_box(area: Rect, layout: Option<&UiLayout>, border: bool) -> Rect {
    let mut result = if border {
        inset(
            area,
            UiEdges {
                top: 1.0,
                right: 1.0,
                bottom: 1.0,
                left: 1.0,
            },
        )
    } else {
        area
    };
    if let Some(padding) = layout.and_then(|layout| layout.padding) {
        result = inset(result, padding);
    }
    result
}

pub(crate) fn gap(layout: Option<&UiLayout>, axis: Axis) -> u16 {
    let value = layout.and_then(|layout| match axis {
        Axis::Horizontal => layout.column_gap.or(layout.gap),
        Axis::Vertical => layout.row_gap.or(layout.gap),
    });
    value.map_or(0, cells)
}

pub(crate) fn vertical(
    area: Rect,
    nodes: &[UiNode],
    preferred_heights: &[u16],
    gap: u16,
) -> Vec<Rect> {
    allocate_axis(area, nodes, preferred_heights, gap, Axis::Vertical)
}

pub(crate) fn horizontal(
    area: Rect,
    nodes: &[UiNode],
    preferred_widths: &[u16],
    gap: u16,
) -> Vec<Rect> {
    allocate_axis(area, nodes, preferred_widths, gap, Axis::Horizontal)
}

fn allocate_axis(
    area: Rect,
    nodes: &[UiNode],
    preferred: &[u16],
    gap: u16,
    axis: Axis,
) -> Vec<Rect> {
    if nodes.is_empty() {
        return Vec::new();
    }
    let available = match axis {
        Axis::Horizontal => area.width,
        Axis::Vertical => area.height,
    };
    let total_gap = gap.saturating_mul(nodes.len().saturating_sub(1) as u16);
    let distributable = available.saturating_sub(total_gap);
    let mut sizes = Vec::with_capacity(nodes.len());
    let mut fixed = 0_u16;
    let mut grow_total = 0.0_f64;
    for (index, node) in nodes.iter().enumerate() {
        let layout = node.props.layout.as_ref();
        let dimension = match axis {
            Axis::Horizontal => layout.and_then(|layout| layout.width.as_ref()),
            Axis::Vertical => layout.and_then(|layout| layout.height.as_ref()),
        };
        let size = dimension
            .and_then(|dimension| resolve_dimension(dimension, distributable))
            .unwrap_or_else(|| preferred.get(index).copied().unwrap_or(1));
        let size = clamp_dimension(size, layout, axis, distributable);
        fixed = fixed.saturating_add(size);
        sizes.push(size);
        grow_total += layout
            .and_then(|layout| layout.grow)
            .unwrap_or(0.0)
            .max(0.0);
    }

    if fixed < distributable {
        let spare = distributable - fixed;
        if grow_total > 0.0 {
            let weights = nodes
                .iter()
                .map(|node| {
                    node.props
                        .layout
                        .as_ref()
                        .and_then(|layout| layout.grow)
                        .unwrap_or(0.0)
                        .max(0.0)
                })
                .collect::<Vec<_>>();
            distribute_weighted(&mut sizes, spare, &weights);
        }
    } else if fixed > distributable {
        shrink_to_fit(nodes, &mut sizes, fixed - distributable);
    }

    let mut cursor = match axis {
        Axis::Horizontal => area.x,
        Axis::Vertical => area.y,
    };
    nodes
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let remaining = match axis {
                Axis::Horizontal => area.right().saturating_sub(cursor),
                Axis::Vertical => area.bottom().saturating_sub(cursor),
            };
            let size = sizes[index].min(remaining);
            let rect = match axis {
                Axis::Horizontal => Rect::new(cursor, area.y, size, area.height),
                Axis::Vertical => Rect::new(area.x, cursor, area.width, size),
            };
            cursor = cursor.saturating_add(size).saturating_add(gap);
            rect
        })
        .collect()
}

fn shrink_to_fit(nodes: &[UiNode], sizes: &mut [u16], mut overflow: u16) {
    while overflow > 0 {
        let mut changed = false;
        for (node, size) in nodes.iter().zip(sizes.iter_mut()).rev() {
            if overflow == 0 {
                break;
            }
            let shrink = node
                .props
                .layout
                .as_ref()
                .and_then(|layout| layout.shrink)
                .unwrap_or(1.0);
            if shrink > 0.0 && *size > 1 {
                *size -= 1;
                overflow -= 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

pub(crate) fn grid(
    area: Rect,
    nodes: &[UiNode],
    columns: &[UiDimension],
    column_gap: u16,
    row_gap: u16,
    row_heights: &[u16],
    narrow: bool,
) -> Vec<Rect> {
    if nodes.is_empty() {
        return Vec::new();
    }
    let column_count = if narrow {
        1
    } else if columns.is_empty() {
        // Auto-fit cards to a legible terminal minimum.
        usize::from((area.width / 24).max(1)).min(nodes.len())
    } else {
        columns.len().min(nodes.len()).max(1)
    };
    let column_widths = grid_tracks(area.width, column_count, columns, column_gap);
    let row_count = nodes.len().div_ceil(column_count);
    let mut preferred_rows = vec![1_u16; row_count];
    for (index, height) in row_heights.iter().copied().enumerate().take(nodes.len()) {
        preferred_rows[index / column_count] = preferred_rows[index / column_count].max(height);
    }
    let pseudo_nodes = (0..row_count).map(|_| UiNode::text("")).collect::<Vec<_>>();
    let rows = vertical(area, &pseudo_nodes, &preferred_rows, row_gap);

    let mut result = Vec::with_capacity(nodes.len());
    for index in 0..nodes.len() {
        let row = index / column_count;
        let column = index % column_count;
        let x_offset = column_widths
            .iter()
            .take(column)
            .copied()
            .fold(0_u16, |sum, width| {
                sum.saturating_add(width).saturating_add(column_gap)
            });
        result.push(Rect::new(
            area.x.saturating_add(x_offset),
            rows[row].y,
            column_widths[column],
            rows[row].height,
        ));
    }
    result
}

fn grid_tracks(available: u16, count: usize, declared: &[UiDimension], gap: u16) -> Vec<u16> {
    let usable = available.saturating_sub(gap.saturating_mul(count.saturating_sub(1) as u16));
    if declared.is_empty() {
        return equal_tracks(usable, count);
    }
    let mut tracks = vec![0_u16; count];
    let mut fixed = 0_u32;
    let mut weights = vec![0.0_f64; count];
    for (index, dimension) in declared.iter().take(count).enumerate() {
        match dimension.unit.as_str() {
            "fr" => weights[index] = dimension.value.max(0.0),
            _ => {
                tracks[index] = resolve_dimension(dimension, usable).unwrap_or(0);
                fixed = fixed.saturating_add(u32::from(tracks[index]));
            }
        }
    }

    // Explicit cell/percent tracks are requests, not permission to paint past
    // the viewport. Shrink them deterministically before assigning fractions.
    let mut overflow = fixed.saturating_sub(u32::from(usable));
    for (index, track) in tracks.iter_mut().enumerate().rev() {
        if overflow == 0 {
            break;
        }
        if weights[index] == 0.0 {
            let reduction = u32::from(*track).min(overflow) as u16;
            *track -= reduction;
            overflow -= u32::from(reduction);
        }
    }

    let assigned = tracks.iter().copied().fold(0_u16, u16::saturating_add);
    let remaining = usable.saturating_sub(assigned);
    if weights.iter().any(|weight| *weight > 0.0) {
        distribute_weighted(&mut tracks, remaining, &weights);
    }
    tracks
}

fn equal_tracks(available: u16, count: usize) -> Vec<u16> {
    let count_u16 = u16::try_from(count).unwrap_or(u16::MAX).max(1);
    let each = available / count_u16;
    let mut result = vec![each; count];
    let remainder = available.saturating_sub(each.saturating_mul(count_u16));
    for track in result.iter_mut().take(usize::from(remainder)) {
        *track = track.saturating_add(1);
    }
    result
}

fn distribute_weighted(sizes: &mut [u16], total: u16, weights: &[f64]) {
    let weight_total = weights
        .iter()
        .copied()
        .filter(|weight| *weight > 0.0)
        .sum::<f64>();
    if total == 0 || weight_total <= 0.0 {
        return;
    }
    let mut assigned = 0_u16;
    let mut fractions = Vec::new();
    for (index, weight) in weights.iter().copied().enumerate() {
        if weight <= 0.0 {
            continue;
        }
        let exact = f64::from(total) * weight / weight_total;
        let addition = exact.floor() as u16;
        sizes[index] = sizes[index].saturating_add(addition);
        assigned = assigned.saturating_add(addition);
        fractions.push((index, exact - exact.floor()));
    }
    fractions.sort_by(|(left_index, left), (right_index, right)| {
        right
            .total_cmp(left)
            .then_with(|| left_index.cmp(right_index))
    });
    for (index, _) in fractions
        .into_iter()
        .take(usize::from(total.saturating_sub(assigned)))
    {
        sizes[index] = sizes[index].saturating_add(1);
    }
}

pub(crate) fn resolve_dimension(dimension: &UiDimension, parent: u16) -> Option<u16> {
    if !dimension.value.is_finite() || dimension.value < 0.0 {
        return None;
    }
    let value = match dimension.unit.as_str() {
        "cells" | "cell" | "px" => dimension.value,
        "percent" | "%" => f64::from(parent) * dimension.value / 100.0,
        "auto" | "fr" => return None,
        _ => return None,
    };
    Some(value.round().clamp(0.0, f64::from(u16::MAX)) as u16)
}

fn clamp_dimension(value: u16, layout: Option<&UiLayout>, axis: Axis, parent: u16) -> u16 {
    let (minimum, maximum) = layout.map_or((None, None), |layout| match axis {
        Axis::Horizontal => (layout.min_width.as_ref(), layout.max_width.as_ref()),
        Axis::Vertical => (layout.min_height.as_ref(), layout.max_height.as_ref()),
    });
    let minimum = minimum
        .and_then(|value| resolve_dimension(value, parent))
        .unwrap_or(0);
    let maximum = maximum
        .and_then(|value| resolve_dimension(value, parent))
        .unwrap_or(parent);
    value.max(minimum).min(maximum)
}

fn cells(value: f64) -> u16 {
    if value.is_finite() && value > 0.0 {
        value.round().clamp(0.0, f64::from(u16::MAX)) as u16
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_protocol::remote_ui::{UiDimension, UiNode};

    #[test]
    fn horizontal_layout_is_bounded_and_deterministic() {
        let nodes = vec![UiNode::text("a"), UiNode::text("b"), UiNode::text("c")];
        let rects = horizontal(Rect::new(2, 3, 11, 4), &nodes, &[3, 3, 3], 1);
        assert_eq!(
            rects,
            [
                Rect::new(2, 3, 3, 4),
                Rect::new(6, 3, 3, 4),
                Rect::new(10, 3, 3, 4)
            ]
        );
        assert!(rects.iter().all(|rect| rect.right() <= 13));
    }

    #[test]
    fn grid_collapses_to_one_column_when_narrow() {
        let nodes = vec![UiNode::text("a"), UiNode::text("b")];
        let rows = grid(Rect::new(0, 0, 20, 5), &nodes, &[], 1, 1, &[1, 1], true);
        assert_eq!(rows[0], Rect::new(0, 0, 20, 1));
        assert_eq!(rows[1], Rect::new(0, 2, 20, 1));
    }

    #[test]
    fn dimensions_resolve_cells_and_percent() {
        assert_eq!(
            resolve_dimension(
                &UiDimension {
                    value: 50.0,
                    unit: "percent".into()
                },
                80
            ),
            Some(40)
        );
    }

    #[test]
    fn grow_remainder_only_goes_to_growing_children() {
        let mut growing = UiNode::text("growing");
        growing.props.layout = Some(UiLayout {
            grow: Some(1.0),
            ..UiLayout::default()
        });
        let fixed = UiNode::text("fixed");
        let rects = horizontal(Rect::new(0, 0, 5, 1), &[growing, fixed], &[1, 1], 0);
        assert_eq!(rects[0].width, 4);
        assert_eq!(rects[1].width, 1);
    }

    #[test]
    fn fixed_grid_tracks_never_overflow_the_available_width() {
        let tracks = grid_tracks(
            10,
            2,
            &[
                UiDimension {
                    value: 8.0,
                    unit: "cells".into(),
                },
                UiDimension {
                    value: 8.0,
                    unit: "cells".into(),
                },
            ],
            0,
        );
        assert_eq!(tracks.iter().copied().sum::<u16>(), 10);
        assert_eq!(
            grid_tracks(
                10,
                2,
                &[
                    UiDimension {
                        value: 2.0,
                        unit: "cells".into()
                    },
                    UiDimension {
                        value: 2.0,
                        unit: "cells".into()
                    },
                ],
                0
            ),
            vec![2, 2]
        );
    }

    #[test]
    fn grid_remainders_are_distributed_from_the_first_track() {
        assert_eq!(equal_tracks(10, 3), vec![4, 3, 3]);
        let fractions = vec![
            UiDimension {
                value: 1.0,
                unit: "fr".into(),
            },
            UiDimension {
                value: 1.0,
                unit: "fr".into(),
            },
            UiDimension {
                value: 1.0,
                unit: "fr".into(),
            },
        ];
        assert_eq!(grid_tracks(10, 3, &fractions, 0), vec![4, 3, 3]);
    }
}
