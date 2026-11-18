//! One gap's label packing and vertical tracks. This module does not know about graphs.

pub(crate) const TRACK: f32 = 6.0;
pub(crate) const ZONE_PAD: f32 = 8.0;
pub(crate) const LABEL_GAP: f32 = 10.0;
pub(crate) const LINE_OFFSET: f32 = 2.0;
pub(crate) const PORT_MIN: f32 = 6.0;
pub(crate) const CLEAR: f32 = 8.0;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct EdgeId(pub usize);

#[derive(Clone, Debug)]
pub(crate) enum NetKind {
    First {
        label_w: f32,
        label_h: f32,
    },
    Through,
    SelfLoop {
        port_b: f32,
        label_w: f32,
        label_h: f32,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct Net {
    pub edge: EdgeId,
    pub kind: NetKind,
    pub y_in: f32,
    pub y_out: f32,
    pub travel_y: f32,
    pub return_y: Option<f32>,
}

#[derive(Clone, Debug)]
pub(crate) struct ChannelPlan {
    pub label_rects: Vec<(EdgeId, super::Rect)>,
    pub left_tracks: Vec<(EdgeId, usize)>,
    pub right_tracks: Vec<(EdgeId, usize)>,
    pub n_left: usize,
    pub n_right: usize,
    pub max_label_w: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TrackSpan {
    pub edge: EdgeId,
    pub y_in: f32,
    pub y_out: f32,
}

pub(crate) fn plan_channel(nets: &mut [Net]) -> ChannelPlan {
    let bands = bands_from(nets);
    let labels = pack_labels(nets, &bands);
    let (mut left_spans, mut right_spans) = spans_from(nets);
    let (mut left_tracks, mut n_left) = color_spans(&left_spans);
    let (mut right_tracks, mut n_right) = color_spans(&right_spans);
    if track_cycle(&left_spans) || track_cycle(&right_spans) {
        if let Some(net) = nets
            .iter_mut()
            .filter(|net| matches!(net.kind, NetKind::Through))
            .max_by_key(|net| net.edge)
        {
            net.travel_y = snap(net.travel_y + 4.0);
        }
        (left_spans, right_spans) = spans_from(nets);
        (left_tracks, n_left) = color_spans(&left_spans);
        (right_tracks, n_right) = color_spans(&right_spans);
    }
    let max_label_w = labels.iter().map(|(_, rect)| rect.w).fold(0.0, f32::max);
    ChannelPlan {
        label_rects: labels,
        left_tracks,
        right_tracks,
        n_left,
        n_right,
        max_label_w,
    }
}

fn bands_from(nets: &[Net]) -> Vec<(f32, f32)> {
    let mut bands = Vec::new();
    for net in nets {
        if matches!(net.kind, NetKind::Through) {
            bands.push((net.travel_y - CLEAR, net.travel_y + CLEAR));
        }
        bands.push((net.y_out - 1.0, net.y_out + 5.0));
    }
    bands
}

fn pack_labels(nets: &mut [Net], bands: &[(f32, f32)]) -> Vec<(EdgeId, super::Rect)> {
    let mut items: Vec<usize> = nets
        .iter()
        .enumerate()
        .filter(|(_, net)| label_size(net).is_some())
        .map(|(index, _)| index)
        .collect();
    items.sort_by(|left, right| {
        let left_top = ideal_top(&nets[*left]);
        let right_top = ideal_top(&nets[*right]);
        left_top
            .total_cmp(&right_top)
            .then(nets[*left].edge.cmp(&nets[*right].edge))
    });
    let mut tops = vec![0.0f32; nets.len()];
    let mut previous_bottom = f32::NEG_INFINITY;
    for index in &items {
        let (width, height) = label_size(&nets[*index]).unwrap();
        let mut top = ideal_top(&nets[*index]).max(previous_bottom + LABEL_GAP);
        if matches!(nets[*index].kind, NetKind::SelfLoop { .. }) {
            top = top.max(previous_bottom + LABEL_GAP + LINE_OFFSET);
        }
        top = top.max(36.0);
        let limit = bands.len() + 1;
        for _ in 0..limit {
            if let Some(below) = band_block(
                top,
                occupy(height, &nets[*index].kind),
                bands,
                nets[*index].y_out,
            ) {
                let next = snap(below);
                if next <= top {
                    top = snap(top + 4.0);
                } else {
                    top = next;
                }
            } else {
                break;
            }
        }
        tops[*index] = top;
        previous_bottom = stack_bottom(top, height, &nets[*index].kind);
        let _ = width;
    }
    for cursor in (0..items.len()).rev() {
        let index = items[cursor];
        let (_, height) = label_size(&nets[index]).unwrap();
        let next_top = items.get(cursor + 1).map(|next| tops[*next]);
        let prev_bottom = if cursor == 0 {
            f32::NEG_INFINITY
        } else {
            let prev = items[cursor - 1];
            let (_, prev_h) = label_size(&nets[prev]).unwrap();
            stack_bottom(tops[prev], prev_h, &nets[prev].kind)
        };
        let mut ceiling = next_top
            .map(|top| top - LABEL_GAP - occupy(height, &nets[index].kind))
            .unwrap_or(f32::INFINITY);
        if matches!(nets[index].kind, NetKind::SelfLoop { .. }) {
            ceiling = next_top
                .map(|top| top - LABEL_GAP - LINE_OFFSET - height)
                .unwrap_or(ceiling);
        }
        let floor = if prev_bottom.is_finite() {
            let mut floor = prev_bottom + LABEL_GAP;
            if matches!(nets[index].kind, NetKind::SelfLoop { .. }) {
                floor += LINE_OFFSET;
            }
            floor
        } else {
            36.0
        };
        let mut candidate = ideal_top(&nets[index])
            .max(floor)
            .min(ceiling)
            .min(tops[index]);
        candidate = candidate.max(36.0);
        if band_block(
            candidate,
            occupy(height, &nets[index].kind),
            bands,
            nets[index].y_out,
        )
        .is_none()
        {
            tops[index] = candidate;
        }
    }
    let mut rects = Vec::new();
    for index in &items {
        let (width, height) = label_size(&nets[*index]).unwrap();
        let top = tops[*index];
        nets[*index].travel_y = snap(top - LINE_OFFSET);
        if matches!(nets[*index].kind, NetKind::SelfLoop { .. }) {
            nets[*index].return_y = Some(snap(top + height + LINE_OFFSET));
        }
        rects.push((
            nets[*index].edge,
            super::Rect {
                x: 0.0,
                y: snap(top),
                w: snap(width),
                h: snap(height),
            },
        ));
    }
    rects
}

fn label_size(net: &Net) -> Option<(f32, f32)> {
    match net.kind {
        NetKind::First { label_w, label_h }
        | NetKind::SelfLoop {
            label_w, label_h, ..
        } => Some((label_w, label_h)),
        NetKind::Through => None,
    }
}

fn ideal_top(net: &Net) -> f32 {
    snap(net.y_in + LINE_OFFSET)
}

fn occupy(height: f32, kind: &NetKind) -> f32 {
    if matches!(kind, NetKind::SelfLoop { .. }) {
        height + 2.0 * LINE_OFFSET
    } else {
        height
    }
}

fn stack_bottom(top: f32, height: f32, kind: &NetKind) -> f32 {
    if matches!(kind, NetKind::SelfLoop { .. }) {
        top + height + LINE_OFFSET
    } else {
        top + height
    }
}

fn band_block(top: f32, occupy_h: f32, bands: &[(f32, f32)], y_out: f32) -> Option<f32> {
    let mut blocked = None;
    let bottom = top + occupy_h;
    for (lo, hi) in bands {
        if top < *hi && bottom > *lo {
            blocked = Some(blocked.unwrap_or(*hi).max(*hi));
        }
    }
    let line = top - LINE_OFFSET;
    if (line - y_out).abs() < 3.0 {
        blocked = Some(blocked.unwrap_or(y_out + 5.0).max(y_out + 5.0));
    }
    blocked
}

fn spans_from(nets: &[Net]) -> (Vec<TrackSpan>, Vec<TrackSpan>) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    for net in nets {
        match &net.kind {
            NetKind::First { .. } | NetKind::Through => {
                push_span(&mut left, net.edge, net.y_in, net.travel_y);
                push_span(&mut right, net.edge, net.travel_y, net.y_out);
            }
            NetKind::SelfLoop { .. } => {
                push_span(&mut left, net.edge, net.y_in, net.travel_y);
                let back = net.return_y.unwrap_or(net.y_out);
                push_span(&mut right, net.edge, net.travel_y, back);
                push_span(&mut left, net.edge, back, net.y_out);
            }
        }
    }
    (left, right)
}

fn push_span(out: &mut Vec<TrackSpan>, edge: EdgeId, y_in: f32, y_out: f32) {
    if (y_in - y_out).abs() < 0.5 {
        return;
    }
    out.push(TrackSpan { edge, y_in, y_out });
}

fn color_spans(spans: &[TrackSpan]) -> (Vec<(EdgeId, usize)>, usize) {
    if spans.is_empty() {
        return (Vec::new(), 0);
    }
    let order = track_order(spans);
    let tracks = assign_tracks(&order, spans);
    let n = tracks.iter().copied().max().unwrap_or(0) + 1;
    let pairs = order
        .into_iter()
        .map(|index| (spans[index].edge, tracks[index]))
        .collect();
    (pairs, n)
}

fn track_cycle(spans: &[TrackSpan]) -> bool {
    let order = track_order(spans);
    cycle_pairs(spans, &order).len() > spans.len()
}

fn track_order(spans: &[TrackSpan]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&left, &right| span_key(&spans[left]).cmp(&span_key(&spans[right])));
    for _ in 0..spans.len() {
        let mut moved = false;
        let snapshot = order.clone();
        for left_pos in 0..snapshot.len() {
            for right_pos in 0..snapshot.len() {
                if left_pos == right_pos {
                    continue;
                }
                let a = snapshot[left_pos];
                let b = snapshot[right_pos];
                if (spans[b].y_in - spans[a].y_out).abs() < 2.0 {
                    let pos_a = order.iter().position(|index| *index == a);
                    let pos_b = order.iter().position(|index| *index == b);
                    if let (Some(pos_a), Some(pos_b)) = (pos_a, pos_b) {
                        if pos_b > pos_a {
                            let value = order.remove(pos_b);
                            order.insert(pos_a, value);
                            moved = true;
                        }
                    }
                }
            }
        }
        if !moved {
            break;
        }
    }
    order
}

fn span_key(span: &TrackSpan) -> (u8, i32, i32, usize) {
    let y_in = (span.y_in * 10.0).round() as i32;
    let y_out = (span.y_out * 10.0).round() as i32;
    if span.y_out > span.y_in {
        (0, -y_in, -y_out, span.edge.0)
    } else {
        (1, y_in, y_out, span.edge.0)
    }
}

fn assign_tracks(order: &[usize], spans: &[TrackSpan]) -> Vec<usize> {
    let mut tracks = vec![0usize; spans.len()];
    for (placed, index) in order.iter().copied().enumerate() {
        let mut best = None;
        for earlier in order.iter().copied().take(placed) {
            if spans_overlap(&spans[index], &spans[earlier]) {
                best = Some(best.unwrap_or(tracks[earlier]).max(tracks[earlier]));
            }
        }
        tracks[index] = best.map(|track| track + 1).unwrap_or(0);
    }
    tracks
}

fn spans_overlap(left: &TrackSpan, right: &TrackSpan) -> bool {
    let left_lo = left.y_in.min(left.y_out) - 4.0;
    let left_hi = left.y_in.max(left.y_out) + 4.0;
    let right_lo = right.y_in.min(right.y_out) - 4.0;
    let right_hi = right.y_in.max(right.y_out) + 4.0;
    left_lo < right_hi && right_lo < left_hi
}

fn cycle_pairs(spans: &[TrackSpan], order: &[usize]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for left_pos in 0..order.len() {
        for right_pos in (left_pos + 1)..order.len() {
            let a = order[left_pos];
            let b = order[right_pos];
            if (spans[b].y_in - spans[a].y_out).abs() < 2.0 && right_pos > left_pos {
                pairs.push((a, b));
            }
        }
    }
    pairs
}

fn snap(value: f32) -> f32 {
    (value * 10.0).round() / 10.0
}

pub(crate) fn track_x(zone_left: f32, track: usize) -> f32 {
    snap(zone_left + ZONE_PAD + TRACK * track as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_tracks_never_share_overlapping_spans() {
        let mut rng = 0x1234_5678_u64;
        for _ in 0..40 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let count = 2 + (rng % 8) as usize;
            let mut spans = Vec::new();
            for index in 0..count {
                let y_in = ((rng >> (index + 1)) % 80) as f32 * 4.0 + 40.0;
                let y_out = y_in + 12.0 + ((rng >> (index + 3)) % 6) as f32 * 8.0;
                spans.push(TrackSpan {
                    edge: EdgeId(index),
                    y_in,
                    y_out,
                });
            }
            let order = track_order(&spans);
            let tracks = assign_tracks(&order, &spans);
            for left in 0..spans.len() {
                for right in (left + 1)..spans.len() {
                    if spans_overlap(&spans[left], &spans[right]) {
                        assert_ne!(tracks[left], tracks[right]);
                    }
                    let left_hi = spans[left].y_in.max(spans[left].y_out);
                    let right_lo = spans[right].y_in.min(spans[right].y_out);
                    let parallel = left_hi + 8.0 < right_lo
                        && spans[left].y_out > spans[left].y_in
                        && spans[right].y_out > spans[right].y_in;
                    if parallel {
                        let left_track = tracks[left] as f32;
                        let right_track = tracks[right] as f32;
                        let crosses = (spans[left].y_in - spans[right].y_in)
                            * (spans[left].y_out - spans[right].y_out)
                            < 0.0
                            && (left_track - right_track).abs() > 0.0;
                        assert!(!crosses);
                    }
                }
            }
        }
    }

    #[test]
    fn channel_pack_labels_respects_gap_and_bands() {
        let mut nets = vec![
            Net {
                edge: EdgeId(1),
                kind: NetKind::First {
                    label_w: 40.0,
                    label_h: 16.0,
                },
                y_in: 80.0,
                y_out: 140.0,
                travel_y: 0.0,
                return_y: None,
            },
            Net {
                edge: EdgeId(0),
                kind: NetKind::First {
                    label_w: 50.0,
                    label_h: 16.0,
                },
                y_in: 50.0,
                y_out: 200.0,
                travel_y: 0.0,
                return_y: None,
            },
            Net {
                edge: EdgeId(2),
                kind: NetKind::Through,
                y_in: 120.0,
                y_out: 160.0,
                travel_y: 120.0,
                return_y: None,
            },
        ];
        let first = plan_channel(&mut nets.clone());
        let second = plan_channel(&mut nets);
        assert_eq!(first.label_rects.len(), second.label_rects.len());
        for ((_, left), (_, right)) in first.label_rects.iter().zip(&second.label_rects) {
            assert!((left.y - right.y).abs() < 0.05);
        }
        let rects: Vec<_> = second.label_rects.iter().map(|(_, rect)| *rect).collect();
        for left in 0..rects.len() {
            for right in (left + 1)..rects.len() {
                let gap = if rects[left].y < rects[right].y {
                    rects[right].y - (rects[left].y + rects[left].h)
                } else {
                    rects[left].y - (rects[right].y + rects[right].h)
                };
                assert!(gap + 0.05 >= LABEL_GAP, "{gap}");
            }
            assert!(rects[left].y + 0.05 >= 36.0);
            let line = rects[left].y - LINE_OFFSET;
            assert!((line - 120.0).abs() >= CLEAR - 0.05);
        }
    }
}
