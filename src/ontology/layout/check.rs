//! Geometry checks for a finished layout. Test-only.

use super::{Layout, Rect};
use crate::ontology::graph::{Edge, Graph};

pub(crate) const X_MAX: f32 = 4.0;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Violations {
    pub label_overlap: usize,
    pub line_through_label: usize,
    pub collinear_unshared: usize,
    pub label_not_own_or_far: usize,
    pub legend_hits: usize,
    pub crossings: usize,
}

pub(crate) fn check_layout(graph: &Graph, layout: &Layout) -> Violations {
    let mut violations = Violations::default();
    for (left_index, left) in layout.edges.iter().enumerate() {
        for node in &layout.nodes {
            if area_overlap(left.label_rect, node.rect) {
                violations.label_overlap += 1;
            }
        }
        for right in layout.edges.iter().skip(left_index + 1) {
            if area_overlap(left.label_rect, right.label_rect) {
                violations.label_overlap += 1;
            }
        }
    }
    for edge in &layout.edges {
        for other in &layout.edges {
            if edge.index == other.index {
                continue;
            }
            for pair in edge.points.windows(2) {
                if segment_hits_rect(pair[0], pair[1], other.label_rect, 0.5) {
                    violations.line_through_label += 1;
                }
            }
        }
    }
    for left_index in 0..layout.edges.len() {
        for right_index in (left_index + 1)..layout.edges.len() {
            let left = &layout.edges[left_index];
            let right = &layout.edges[right_index];
            if shares_endpoint(&graph.edges[left.index], &graph.edges[right.index]) {
                continue;
            }
            for left_seg in left.points.windows(2) {
                for right_seg in right.points.windows(2) {
                    if collinear(left_seg[0], left_seg[1], right_seg[0], right_seg[1]) {
                        violations.collinear_unshared += 1;
                    }
                }
            }
        }
    }
    for edge in &layout.edges {
        let mut own = f32::MAX;
        let mut nearest = f32::MAX;
        let mut nearest_own = false;
        for other in &layout.edges {
            for pair in other.points.windows(2) {
                let distance = rect_segment_distance(edge.label_rect, pair[0], pair[1]);
                if distance < nearest {
                    nearest = distance;
                    nearest_own = other.index == edge.index;
                }
                if other.index == edge.index {
                    own = own.min(distance);
                }
            }
        }
        if !nearest_own || own > X_MAX {
            violations.label_not_own_or_far += 1;
        }
    }
    for edge in &layout.edges {
        for pair in edge.points.windows(2) {
            if segment_hits_rect(pair[0], pair[1], layout.legend, 0.0) {
                violations.legend_hits += 1;
            }
        }
        if area_overlap(edge.label_rect, layout.legend) {
            violations.legend_hits += 1;
        }
    }
    for node in &layout.nodes {
        if area_overlap(node.rect, layout.legend) {
            violations.legend_hits += 1;
        }
    }
    violations.crossings = count_crossings(layout);
    violations
}

fn count_crossings(layout: &Layout) -> usize {
    let mut count = 0;
    for left_index in 0..layout.edges.len() {
        for right_index in (left_index + 1)..layout.edges.len() {
            let left = &layout.edges[left_index];
            let right = &layout.edges[right_index];
            for left_seg in left.points.windows(2) {
                for right_seg in right.points.windows(2) {
                    if proper_cross(left_seg[0], left_seg[1], right_seg[0], right_seg[1]) {
                        count += 1;
                    }
                }
            }
        }
    }
    count
}

fn proper_cross(a: (f32, f32), b: (f32, f32), c: (f32, f32), d: (f32, f32)) -> bool {
    let a_h = (a.1 - b.1).abs() < 0.05;
    let c_h = (c.1 - d.1).abs() < 0.05;
    let a_v = (a.0 - b.0).abs() < 0.05;
    let c_v = (c.0 - d.0).abs() < 0.05;
    if a_h && c_v {
        return cross_hv(a, b, c, d);
    }
    if a_v && c_h {
        return cross_hv(c, d, a, b);
    }
    false
}

fn cross_hv(h0: (f32, f32), h1: (f32, f32), v0: (f32, f32), v1: (f32, f32)) -> bool {
    let y = h0.1;
    let x = v0.0;
    let h_lo = h0.0.min(h1.0);
    let h_hi = h0.0.max(h1.0);
    let v_lo = v0.1.min(v1.1);
    let v_hi = v0.1.max(v1.1);
    x > h_lo + 0.5 && x < h_hi - 0.5 && y > v_lo + 0.5 && y < v_hi - 0.5
}

fn area_overlap(left: Rect, right: Rect) -> bool {
    let x = left.x.max(right.x);
    let y = left.y.max(right.y);
    let far_x = (left.x + left.w).min(right.x + right.w);
    let far_y = (left.y + left.h).min(right.y + right.h);
    far_x - x > 0.2 && far_y - y > 0.2
}

fn shares_endpoint(left: &Edge, right: &Edge) -> bool {
    left.from == right.from || left.from == right.to || left.to == right.from || left.to == right.to
}

fn collinear(start: (f32, f32), end: (f32, f32), other: (f32, f32), last: (f32, f32)) -> bool {
    if (start.1 - end.1).abs() < 0.05
        && (other.1 - last.1).abs() < 0.05
        && (start.1 - other.1).abs() < 0.5
    {
        let low = start.0.min(end.0).max(other.0.min(last.0));
        let high = start.0.max(end.0).min(other.0.max(last.0));
        return high - low > 2.0;
    }
    if (start.0 - end.0).abs() < 0.05
        && (other.0 - last.0).abs() < 0.05
        && (start.0 - other.0).abs() < 0.5
    {
        let low = start.1.min(end.1).max(other.1.min(last.1));
        let high = start.1.max(end.1).min(other.1.max(last.1));
        return high - low > 2.0;
    }
    false
}

fn segment_hits_rect(start: (f32, f32), end: (f32, f32), rect: Rect, shrink: f32) -> bool {
    let x0 = rect.x + shrink;
    let y0 = rect.y + shrink;
    let x1 = rect.x + rect.w - shrink;
    let y1 = rect.y + rect.h - shrink;
    if x1 <= x0 || y1 <= y0 {
        return false;
    }
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    let mut t0 = 0.0f32;
    let mut t1 = 1.0f32;
    for (p, q) in [
        (-dx, start.0 - x0),
        (dx, x1 - start.0),
        (-dy, start.1 - y0),
        (dy, y1 - start.1),
    ] {
        if p.abs() < 0.0001 {
            if q < 0.0 {
                return false;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

fn rect_segment_distance(rect: Rect, start: (f32, f32), end: (f32, f32)) -> f32 {
    if segment_hits_rect(start, end, rect, 0.0) {
        return 0.0;
    }
    let samples = [
        (rect.x, rect.y),
        (rect.x + rect.w, rect.y),
        (rect.x, rect.y + rect.h),
        (rect.x + rect.w, rect.y + rect.h),
        (rect.x + rect.w * 0.5, rect.y),
        (rect.x + rect.w * 0.5, rect.y + rect.h),
    ];
    samples
        .into_iter()
        .map(|point| point_segment_distance(point, start, end))
        .fold(f32::MAX, f32::min)
}

fn point_segment_distance(point: (f32, f32), start: (f32, f32), end: (f32, f32)) -> f32 {
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    let len = dx * dx + dy * dy;
    let t = if len == 0.0 {
        0.0
    } else {
        ((point.0 - start.0) * dx + (point.1 - start.1) * dy) / len
    };
    let t = t.clamp(0.0, 1.0);
    let x = start.0 + t * dx - point.0;
    let y = start.1 + t * dy - point.1;
    (x * x + y * y).sqrt()
}
