//! Turn one edge's gap plans into an orthogonal polyline.

use super::channel::{track_x, Net, NetKind};
use super::Rect;

#[derive(Clone, Copy, Debug)]
pub(crate) struct GapGeometry {
    pub gap_left: f32,
    pub left_zone_x: f32,
    pub label_x: f32,
    pub right_zone_x: f32,
    pub gap_right: f32,
}

pub(crate) fn polyline(
    start: (f32, f32),
    end: (f32, f32),
    legs: &[(GapGeometry, &Net, Option<usize>, Option<usize>)],
) -> Vec<(f32, f32)> {
    let mut points = Vec::new();
    push(&mut points, start);
    for (index, (geo, net, left, right)) in legs.iter().enumerate() {
        let travel = net.travel_y;
        match left {
            Some(track) => {
                let x = track_x(geo.left_zone_x, *track);
                push(&mut points, (x, net.y_in));
                push(&mut points, (x, travel));
            }
            None => push(&mut points, (geo.gap_left, travel)),
        }
        match right {
            Some(track) => {
                let x = track_x(geo.right_zone_x, *track);
                push(&mut points, (x, travel));
                push(&mut points, (x, net.y_out));
            }
            None => push(&mut points, (geo.gap_right, travel)),
        }
        push(&mut points, (geo.gap_right, net.y_out));
        if index + 1 < legs.len() {
            push(&mut points, (legs[index + 1].0.gap_left, net.y_out));
        }
    }
    push(&mut points, end);
    points
}

pub(crate) fn self_loop(
    node: Rect,
    geo: &GapGeometry,
    net: &Net,
    left: (Option<usize>, Option<usize>),
    right: Option<usize>,
) -> Vec<(f32, f32)> {
    let port_b = match &net.kind {
        NetKind::SelfLoop { port_b, .. } => *port_b,
        _ => net.y_out,
    };
    let back = net.return_y.unwrap_or(net.y_out);
    let node_right = snap(node.x + node.w);
    let mut points = Vec::new();
    push(&mut points, (node_right, net.y_in));
    if let Some(track) = left.0 {
        let x = track_x(geo.left_zone_x, track);
        push(&mut points, (x, net.y_in));
        push(&mut points, (x, net.travel_y));
    }
    let right_x = right.map(|track| track_x(geo.right_zone_x, track));
    if let Some(x) = right_x {
        push(&mut points, (x, net.travel_y));
        push(&mut points, (x, back));
    } else {
        push(&mut points, (geo.gap_right, net.travel_y));
        push(&mut points, (geo.gap_right, back));
    }
    if let Some(track) = left.1 {
        let x = track_x(geo.left_zone_x, track);
        push(&mut points, (x, back));
        push(&mut points, (x, port_b));
    }
    push(&mut points, (node_right, port_b));
    points
}

fn push(points: &mut Vec<(f32, f32)>, point: (f32, f32)) {
    let point = (snap(point.0), snap(point.1));
    if let Some(previous) = points.last() {
        if (previous.0 - point.0).abs() < 0.05 && (previous.1 - point.1).abs() < 0.05 {
            return;
        }
    }
    points.push(point);
}

fn snap(value: f32) -> f32 {
    (value * 10.0).round() / 10.0
}
