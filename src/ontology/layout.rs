//! Deterministic left-to-right layout. Coordinates are rounded to 0.1 px.

use std::collections::{BTreeMap, BTreeSet};

use crate::ontology::graph::{Edge, EdgeSource, Graph, Node, NodeKind, TableKey};

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Debug)]
pub struct PlacedNode {
    pub key: TableKey,
    pub rect: Rect,
    #[allow(dead_code)]
    pub layer: usize,
}

#[derive(Clone, Debug)]
pub struct PlacedEdge {
    pub index: usize,
    pub points: Vec<(f32, f32)>,
    pub label_rect: Rect,
    pub label_lines: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub width: f32,
    pub height: f32,
    pub nodes: Vec<PlacedNode>,
    pub edges: Vec<PlacedEdge>,
}

pub(crate) fn text_width(text: &str, font_px: f32) -> f32 {
    text.chars()
        .map(|ch| {
            if wide_char(ch) {
                font_px
            } else {
                font_px * 0.6
            }
        })
        .sum()
}

pub fn layout(graph: &Graph) -> Layout {
    let keys: Vec<TableKey> = graph.nodes.keys().cloned().collect();
    let index_of: BTreeMap<TableKey, usize> = keys
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, key)| (key, index))
        .collect();
    let lines: Vec<Vec<String>> = keys
        .iter()
        .map(|key| node_lines(&graph.nodes[key]))
        .collect();
    let sizes: Vec<(f32, f32)> = lines
        .iter()
        .map(|lines| {
            let width = lines
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    let font = if index == 0 { 13.0 } else { 12.0 };
                    text_width(line, font)
                })
                .fold(80.0, f32::max)
                + 24.0;
            let height = 16.0 + lines.len() as f32 * 18.0;
            (snap(width), snap(height))
        })
        .collect();

    let mut forward: Vec<bool> = vec![true; graph.edges.len()];
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); keys.len()];
    let mut self_loops = Vec::new();
    for (index, edge) in graph.edges.iter().enumerate() {
        if edge.from == edge.to {
            self_loops.push(index);
            continue;
        }
        let from = index_of[&edge.from];
        let to = index_of[&edge.to];
        adjacency[from].push(index);
        let _ = to;
    }
    for neighbors in &mut adjacency {
        neighbors.sort_by_key(|index| {
            (
                graph.edges[*index].to.clone(),
                graph.edges[*index].columns.clone(),
            )
        });
    }
    let back = back_edges(&keys, &index_of, graph);
    let mut dag: Vec<Vec<usize>> = vec![Vec::new(); keys.len()];
    for (index, edge) in graph.edges.iter().enumerate() {
        if edge.from == edge.to || back.contains(&index) {
            if back.contains(&index) {
                forward[index] = false;
                let from = index_of[&edge.to];
                dag[from].push(index);
            }
            continue;
        }
        dag[index_of[&edge.from]].push(index);
    }

    let mut layer = vec![0usize; keys.len()];
    for node in topo(&dag, graph, &index_of, keys.len()) {
        for edge_index in &dag[node] {
            let edge = &graph.edges[*edge_index];
            let child = if forward[*edge_index] {
                index_of[&edge.to]
            } else {
                index_of[&edge.from]
            };
            layer[child] = layer[child].max(layer[node] + 1);
        }
    }

    let mut slots: Vec<Vec<Slot>> = Vec::new();
    let max_layer = layer.iter().copied().max().unwrap_or(0);
    slots.resize(max_layer + 1, Vec::new());
    for (index, key) in keys.iter().enumerate() {
        slots[layer[index]].push(Slot {
            name: key.title(),
            node: Some(index),
            edge: None,
            width: sizes[index].0,
            height: sizes[index].1,
        });
    }
    let mut dummies: Vec<(usize, usize, usize)> = Vec::new();
    for (index, edge) in graph.edges.iter().enumerate() {
        if edge.from == edge.to {
            continue;
        }
        let (left, right) = span(index, edge, &index_of, &forward, &layer);
        if right > left + 1 {
            let mut step = left + 1;
            while step < right {
                let dummy_at = slots[step].len();
                slots[step].push(Slot {
                    name: format!("~{index}-{step}"),
                    node: None,
                    edge: Some(index),
                    width: 0.0,
                    height: 8.0,
                });
                dummies.push((index, step, dummy_at));
                step += 1;
            }
        }
    }
    for _ in 0..4 {
        order_layer(&mut slots, graph, &index_of, &forward, true);
        order_layer(&mut slots, graph, &index_of, &forward, false);
    }

    let label_lines: Vec<Vec<String>> = graph.edges.iter().map(edge_label_lines).collect();
    let label_widths: Vec<f32> = label_lines
        .iter()
        .map(|lines| {
            lines
                .iter()
                .map(|line| text_width(line, 12.0))
                .fold(0.0, f32::max)
                + 8.0
        })
        .collect();
    let mut self_loop_pad = vec![0.0f32; slots.len()];
    for edge in &graph.edges {
        if edge.from == edge.to {
            self_loop_pad[layer[index_of[&edge.from]]] = 32.0;
        }
    }
    let gutter_edges = gutter_members(graph, &layer, &index_of, slots.len());
    let mut gap = vec![120.0f32; slots.len().saturating_sub(1)];
    for (gutter, members) in gutter_edges.iter().enumerate() {
        if gutter >= gap.len() {
            continue;
        }
        let label_w =
            label_width_in_gutter(gutter, members, &label_widths, graph, &layer, &index_of);
        let n = members.len() as f32;
        let pad = self_loop_pad[gutter];
        gap[gutter] = gap[gutter]
            .max(label_w + 48.0 + pad)
            .max(pad + label_w + 8.0 * n + 28.0)
            .max(120.0);
    }

    let mut layer_x = vec![16.0f32; slots.len()];
    let mut layer_w = vec![0.0f32; slots.len()];
    for (index, column) in slots.iter().enumerate() {
        layer_w[index] = column.iter().map(|slot| slot.width).fold(0.0, f32::max);
        if index > 0 {
            layer_x[index] = snap(layer_x[index - 1] + layer_w[index - 1] + gap[index - 1]);
        }
    }
    let mut placed_slots: Vec<Vec<Rect>> = Vec::new();
    let mut column_height = Vec::new();
    for column in &slots {
        let mut y = 40.0;
        let mut rects = Vec::new();
        for slot in column {
            rects.push(Rect {
                x: 0.0,
                y: snap(y),
                w: slot.width,
                h: slot.height,
            });
            y += slot.height + 24.0;
        }
        column_height.push(y);
        placed_slots.push(rects);
    }
    let tallest = column_height.iter().copied().fold(40.0, f32::max);
    for (column, rects) in slots.iter().zip(placed_slots.iter_mut()) {
        let used = if column.is_empty() {
            0.0
        } else {
            let bottom = rects.last().map(|rect| rect.y + rect.h).unwrap_or(40.0);
            bottom - 40.0
        };
        let shift = snap(((tallest - 40.0) - used).max(0.0) / 2.0);
        for (slot, rect) in column.iter().zip(rects.iter_mut()) {
            rect.x = snap(layer_x[layer_of(slot, &layer, &index_of, graph)]);
            rect.y = snap(rect.y + shift);
            if slot.node.is_none() {
                rect.x = snap(layer_x[dummy_layer(slot, &dummies, &slots)]);
            }
        }
    }
    // Recompute x from the slot's layer directly.
    for (layer_index, column) in slots.iter().enumerate() {
        for (slot_index, slot) in column.iter().enumerate() {
            placed_slots[layer_index][slot_index].x = snap(layer_x[layer_index]);
            if let Some(node_index) = slot.node {
                let extra = layer_w[layer_index] - sizes[node_index].0;
                let _ = extra;
                placed_slots[layer_index][slot_index].w = sizes[node_index].0;
            }
        }
    }

    let mut node_rect: BTreeMap<TableKey, Rect> = BTreeMap::new();
    let mut node_layer: BTreeMap<TableKey, usize> = BTreeMap::new();
    for (layer_index, column) in slots.iter().enumerate() {
        for (slot_index, slot) in column.iter().enumerate() {
            if let Some(node_index) = slot.node {
                let key = keys[node_index].clone();
                node_rect.insert(key.clone(), placed_slots[layer_index][slot_index]);
                node_layer.insert(key, layer_index);
            }
        }
    }

    let label_rects = place_labels(
        graph,
        &label_lines,
        &label_widths,
        &node_rect,
        &layer,
        &index_of,
        &layer_x,
        &layer_w,
        &self_loop_pad,
        &gutter_edges,
    );
    let mut placed_edges = Vec::new();
    for (index, edge) in graph.edges.iter().enumerate() {
        let points = if edge.from == edge.to {
            loop_points(&node_rect[&edge.from])
        } else {
            route_edge(
                index,
                edge,
                graph,
                &slots,
                &placed_slots,
                &node_rect,
                &layer,
                &index_of,
                &layer_x,
                &layer_w,
                &self_loop_pad,
                &label_rects,
                &gutter_edges,
                &label_widths,
            )
        };
        placed_edges.push(PlacedEdge {
            index,
            points,
            label_rect: label_rects[index],
            label_lines: label_lines[index].clone(),
        });
    }

    let mut width = 32.0_f32;
    let mut height = 80.0_f32;
    for rect in node_rect.values() {
        width = width.max(rect.x + rect.w + 48.0);
        height = height.max(rect.y + rect.h + 72.0);
    }
    for edge in &placed_edges {
        for point in &edge.points {
            width = width.max(point.0 + 24.0);
            height = height.max(point.1 + 24.0);
        }
        width = width.max(edge.label_rect.x + edge.label_rect.w + 16.0);
        height = height.max(edge.label_rect.y + edge.label_rect.h + 48.0);
    }
    Layout {
        width: snap(width),
        height: snap(height),
        nodes: keys
            .into_iter()
            .map(|key| PlacedNode {
                layer: node_layer[&key],
                rect: node_rect[&key],
                key,
            })
            .collect(),
        edges: placed_edges,
    }
}

#[derive(Clone)]
struct Slot {
    name: String,
    node: Option<usize>,
    edge: Option<usize>,
    width: f32,
    height: f32,
}

fn layer_of(
    slot: &Slot,
    layer: &[usize],
    index_of: &BTreeMap<TableKey, usize>,
    graph: &Graph,
) -> usize {
    if let Some(node) = slot.node {
        return layer[node];
    }
    let _ = (index_of, graph);
    0
}

fn dummy_layer(slot: &Slot, dummies: &[(usize, usize, usize)], slots: &[Vec<Slot>]) -> usize {
    for (layer_index, column) in slots.iter().enumerate() {
        if column.iter().any(|item| item.name == slot.name) {
            return layer_index;
        }
    }
    let _ = dummies;
    0
}

fn back_edges(
    keys: &[TableKey],
    index_of: &BTreeMap<TableKey, usize>,
    graph: &Graph,
) -> BTreeSet<usize> {
    let mut outgoing: Vec<Vec<usize>> = vec![Vec::new(); keys.len()];
    for (index, edge) in graph.edges.iter().enumerate() {
        if edge.from == edge.to {
            continue;
        }
        outgoing[index_of[&edge.from]].push(index);
    }
    for list in &mut outgoing {
        list.sort_by_key(|index| graph.edges[*index].to.clone());
    }
    let mut color = vec![0u8; keys.len()];
    let mut back = BTreeSet::new();
    fn walk(
        node: usize,
        outgoing: &[Vec<usize>],
        graph: &Graph,
        index_of: &BTreeMap<TableKey, usize>,
        color: &mut [u8],
        back: &mut BTreeSet<usize>,
    ) {
        color[node] = 1;
        for edge_index in &outgoing[node] {
            let next = index_of[&graph.edges[*edge_index].to];
            if color[next] == 1 {
                back.insert(*edge_index);
            } else if color[next] == 0 {
                walk(next, outgoing, graph, index_of, color, back);
            }
        }
        color[node] = 2;
    }
    for start in 0..keys.len() {
        if color[start] == 0 {
            walk(start, &outgoing, graph, index_of, &mut color, &mut back);
        }
    }
    back
}

fn topo(
    dag: &[Vec<usize>],
    graph: &Graph,
    index_of: &BTreeMap<TableKey, usize>,
    count: usize,
) -> Vec<usize> {
    let mut indegree = vec![0usize; count];
    let mut next: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (node, edges) in dag.iter().enumerate() {
        for edge_index in edges {
            let edge = &graph.edges[*edge_index];
            let child = if index_of[&edge.from] == node {
                index_of[&edge.to]
            } else {
                index_of[&edge.from]
            };
            if child == node {
                continue;
            }
            next[node].push(child);
            indegree[child] += 1;
        }
    }
    let mut ready: BTreeSet<usize> = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect();
    let mut order = Vec::new();
    while let Some(node) = ready.pop_first() {
        order.push(node);
        for child in &next[node] {
            indegree[*child] -= 1;
            if indegree[*child] == 0 {
                ready.insert(*child);
            }
        }
    }
    for index in 0..count {
        if !order.contains(&index) {
            order.push(index);
        }
    }
    order
}

fn span(
    index: usize,
    edge: &Edge,
    index_of: &BTreeMap<TableKey, usize>,
    forward: &[bool],
    layer: &[usize],
) -> (usize, usize) {
    let from = layer[index_of[&edge.from]];
    let to = layer[index_of[&edge.to]];
    let _ = (index, forward);
    (from.min(to), from.max(to))
}

fn order_layer(
    slots: &mut [Vec<Slot>],
    graph: &Graph,
    index_of: &BTreeMap<TableKey, usize>,
    forward: &[bool],
    downward: bool,
) {
    let positions: Vec<BTreeMap<usize, usize>> = slots
        .iter()
        .map(|column| {
            column
                .iter()
                .enumerate()
                .filter_map(|(position, slot)| slot.node.map(|node| (node, position)))
                .collect()
        })
        .collect();
    for column in slots.iter_mut() {
        column.sort_by(|left, right| {
            let left_weight = barycenter(left, graph, index_of, forward, &positions, downward);
            let right_weight = barycenter(right, graph, index_of, forward, &positions, downward);
            left_weight
                .partial_cmp(&right_weight)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.name.cmp(&right.name))
        });
    }
}

fn barycenter(
    slot: &Slot,
    graph: &Graph,
    index_of: &BTreeMap<TableKey, usize>,
    forward: &[bool],
    positions: &[BTreeMap<usize, usize>],
    downward: bool,
) -> f32 {
    let Some(node) = slot.node else {
        return 1_000_000.0 + slot.edge.unwrap_or(0) as f32;
    };
    let mut total = 0.0;
    let mut count = 0.0;
    for (index, edge) in graph.edges.iter().enumerate() {
        if edge.from == edge.to {
            continue;
        }
        let here = index_of[&edge.from] == node || index_of[&edge.to] == node;
        if !here {
            continue;
        }
        let (left_node, right_node) = if forward[index] {
            (index_of[&edge.from], index_of[&edge.to])
        } else {
            (index_of[&edge.to], index_of[&edge.from])
        };
        let other = if node == left_node {
            right_node
        } else {
            left_node
        };
        let want_other = if downward {
            node == left_node
        } else {
            node == right_node
        };
        if !want_other {
            continue;
        }
        for column in positions {
            if let Some(position) = column.get(&other) {
                total += *position as f32;
                count += 1.0;
            }
        }
    }
    if count == 0.0 {
        node as f32
    } else {
        total / count
    }
}

#[allow(clippy::too_many_arguments)]
fn route_edge(
    index: usize,
    edge: &Edge,
    graph: &Graph,
    slots: &[Vec<Slot>],
    placed: &[Vec<Rect>],
    node_rect: &BTreeMap<TableKey, Rect>,
    layer: &[usize],
    index_of: &BTreeMap<TableKey, usize>,
    layer_x: &[f32],
    layer_w: &[f32],
    pad: &[f32],
    labels: &[Rect],
    gutter_edges: &[Vec<usize>],
    label_widths: &[f32],
) -> Vec<(f32, f32)> {
    let from_layer = layer[index_of[&edge.from]];
    let to_layer = layer[index_of[&edge.to]];
    let left_to_right = from_layer <= to_layer;
    let source = if left_to_right { &edge.from } else { &edge.to };
    let target = if left_to_right { &edge.to } else { &edge.from };
    let source_rect = node_rect[source];
    let target_rect = node_rect[target];
    let source_y = port_y(source, true, index, graph, node_rect, layer, index_of);
    let target_y = port_y(target, false, index, graph, node_rect, layer, index_of);
    let left = from_layer.min(to_layer);
    let right = from_layer.max(to_layer);
    let lane_y = edge_lane_y(index, labels, node_rect, placed);
    let mut points = Vec::new();
    push_point(
        &mut points,
        (snap(source_rect.x + source_rect.w), snap(source_y)),
    );
    let mut cursor_x = snap(source_rect.x + source_rect.w);
    let mut cursor_y = snap(source_y);
    if right > left {
        for layer_index in left..right {
            let gutter_left = snap(layer_x[layer_index] + layer_w[layer_index]);
            let jog = jog_x(
                gutter_left,
                pad[layer_index],
                index,
                &gutter_edges[layer_index],
            );
            let lane = lane_x(
                gutter_left,
                pad[layer_index],
                index,
                layer_index,
                &gutter_edges[layer_index],
                label_widths,
                graph,
                layer,
                index_of,
            );
            let travel_y = lane_y;
            push_point(&mut points, (jog, cursor_y));
            push_point(&mut points, (jog, travel_y));
            push_point(&mut points, (lane, travel_y));
            cursor_x = lane;
            cursor_y = travel_y;
            if layer_index + 1 < right {
                let through = snap(
                    dummy_y(index, layer_index + 1, slots, placed)
                        .unwrap_or(target_y + 4.0 * (index as f32 + 1.0)),
                );
                let exit = snap(layer_x[layer_index + 1] + layer_w[layer_index + 1]);
                push_point(&mut points, (cursor_x, through));
                push_point(&mut points, (exit, through));
                cursor_x = exit;
                cursor_y = through;
            }
        }
    }
    push_point(&mut points, (cursor_x, snap(target_y)));
    push_point(&mut points, (snap(target_rect.x), snap(target_y)));
    if !left_to_right {
        points.reverse();
    }
    points
}

fn gutter_members(
    graph: &Graph,
    layer: &[usize],
    index_of: &BTreeMap<TableKey, usize>,
    layers: usize,
) -> Vec<Vec<usize>> {
    let mut members = vec![Vec::new(); layers];
    for (index, edge) in graph.edges.iter().enumerate() {
        let Some((left, right)) = edge_span(edge, layer, index_of) else {
            continue;
        };
        let mut gutter = left;
        while gutter < right && gutter < members.len() {
            members[gutter].push(index);
            gutter += 1;
        }
    }
    members
}

fn edge_span(
    edge: &Edge,
    layer: &[usize],
    index_of: &BTreeMap<TableKey, usize>,
) -> Option<(usize, usize)> {
    if edge.from == edge.to {
        return None;
    }
    let from = layer[index_of[&edge.from]];
    let to = layer[index_of[&edge.to]];
    Some((from.min(to), from.max(to)))
}

fn rank_in(members: &[usize], edge_index: usize) -> usize {
    members
        .iter()
        .position(|index| *index == edge_index)
        .unwrap_or(0)
}

fn label_width_in_gutter(
    gutter: usize,
    members: &[usize],
    label_widths: &[f32],
    graph: &Graph,
    layer: &[usize],
    index_of: &BTreeMap<TableKey, usize>,
) -> f32 {
    let mut width = members
        .iter()
        .filter_map(|index| {
            let (left, _) = edge_span(&graph.edges[*index], layer, index_of)?;
            (left == gutter).then_some(label_widths[*index])
        })
        .fold(0.0, f32::max);
    for (index, edge) in graph.edges.iter().enumerate() {
        if edge.from == edge.to && layer[index_of[&edge.from]] == gutter {
            width = width.max(label_widths[index]);
        }
    }
    width
}

fn jog_x(gutter_left: f32, pad: f32, edge_index: usize, members: &[usize]) -> f32 {
    let rank = rank_in(members, edge_index);
    snap(gutter_left + pad + 4.0 * (rank as f32 + 1.0))
}

fn label_column_x(gutter_left: f32, pad: f32, members: usize) -> f32 {
    snap(gutter_left + pad + 4.0 * (members as f32 + 1.0) + 8.0)
}

#[allow(clippy::too_many_arguments)]
fn lane_x(
    gutter_left: f32,
    pad: f32,
    edge_index: usize,
    gutter: usize,
    members: &[usize],
    label_widths: &[f32],
    graph: &Graph,
    layer: &[usize],
    index_of: &BTreeMap<TableKey, usize>,
) -> f32 {
    let label_w = label_width_in_gutter(gutter, members, label_widths, graph, layer, index_of);
    let origin = label_column_x(gutter_left, pad, members.len()) + label_w + 8.0;
    let rank = rank_in(members, edge_index);
    snap(origin + 4.0 * (rank as f32 + 1.0))
}

fn edge_lane_y(
    edge_index: usize,
    labels: &[Rect],
    node_rect: &BTreeMap<TableKey, Rect>,
    placed: &[Vec<Rect>],
) -> f32 {
    let mut floor = 0.0f32;
    for rect in labels {
        floor = floor.max(rect.y + rect.h);
    }
    for rect in node_rect.values() {
        floor = floor.max(rect.y + rect.h);
    }
    for column in placed {
        for rect in column {
            floor = floor.max(rect.y + rect.h);
        }
    }
    snap(floor + 16.0 + edge_index as f32 * 4.0)
}

fn push_point(points: &mut Vec<(f32, f32)>, point: (f32, f32)) {
    if let Some(previous) = points.last() {
        if (previous.0 - point.0).abs() < 0.05 && (previous.1 - point.1).abs() < 0.05 {
            return;
        }
    }
    points.push(point);
}

fn dummy_y(
    edge: usize,
    layer_index: usize,
    slots: &[Vec<Slot>],
    placed: &[Vec<Rect>],
) -> Option<f32> {
    slots[layer_index]
        .iter()
        .zip(&placed[layer_index])
        .find_map(|(slot, rect)| (slot.edge == Some(edge)).then_some(rect.y + rect.h / 2.0))
}

fn port_y(
    key: &TableKey,
    right_side: bool,
    edge_index: usize,
    graph: &Graph,
    node_rect: &BTreeMap<TableKey, Rect>,
    layer: &[usize],
    index_of: &BTreeMap<TableKey, usize>,
) -> f32 {
    let rect = node_rect[key];
    let mut touching = Vec::new();
    for (index, edge) in graph.edges.iter().enumerate() {
        if edge.from == edge.to {
            continue;
        }
        let from_layer = layer[index_of[&edge.from]];
        let to_layer = layer[index_of[&edge.to]];
        let on_this_side = if from_layer <= to_layer {
            if right_side {
                &edge.from
            } else {
                &edge.to
            }
        } else if right_side {
            &edge.to
        } else {
            &edge.from
        };
        if on_this_side == key {
            touching.push(index);
        }
    }
    let rank = touching
        .iter()
        .position(|index| *index == edge_index)
        .unwrap_or(0);
    let count = touching.len().max(1) as f32;
    snap(rect.y + rect.h * (rank as f32 + 1.0) / (count + 1.0))
}

fn loop_points(rect: &Rect) -> Vec<(f32, f32)> {
    let y1 = snap(rect.y + rect.h * 0.35);
    let y2 = snap(rect.y + rect.h * 0.65);
    let x = snap(rect.x + rect.w);
    let bow = snap(x + 28.0);
    vec![(x, y1), (bow, y1), (bow, y2), (x, y2)]
}

#[allow(clippy::too_many_arguments)]
fn place_labels(
    graph: &Graph,
    label_lines: &[Vec<String>],
    label_widths: &[f32],
    node_rect: &BTreeMap<TableKey, Rect>,
    layer: &[usize],
    index_of: &BTreeMap<TableKey, usize>,
    layer_x: &[f32],
    layer_w: &[f32],
    pad: &[f32],
    gutter_edges: &[Vec<usize>],
) -> Vec<Rect> {
    let boxes: Vec<Rect> = node_rect.values().copied().collect();
    let mut drafts = Vec::new();
    for (index, edge) in graph.edges.iter().enumerate() {
        let gutter = if edge.from == edge.to {
            layer[index_of[&edge.from]]
        } else {
            let from_layer = layer[index_of[&edge.from]];
            let to_layer = layer[index_of[&edge.to]];
            from_layer.min(to_layer)
        };
        let gutter_left = snap(layer_x[gutter] + layer_w[gutter]);
        let x = label_column_x(
            gutter_left,
            pad[gutter],
            gutter_edges.get(gutter).map(Vec::len).unwrap_or(0),
        );
        let ideal_y = if edge.from == edge.to {
            let rect = node_rect[&edge.from];
            snap(rect.y + rect.h * 0.35)
        } else {
            let from_layer = layer[index_of[&edge.from]];
            let to_layer = layer[index_of[&edge.to]];
            let left_key = if from_layer <= to_layer {
                &edge.from
            } else {
                &edge.to
            };
            port_y(left_key, true, index, graph, node_rect, layer, index_of)
        };
        drafts.push((
            gutter,
            ideal_y,
            index,
            Rect {
                x,
                y: ideal_y,
                w: snap(label_widths[index]),
                h: snap(label_lines[index].len() as f32 * 16.0),
            },
        ));
    }
    drafts.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(
                left.1
                    .partial_cmp(&right.1)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(left.2.cmp(&right.2))
    });
    let mut placed: Vec<Rect> = Vec::new();
    let mut rects = vec![
        Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        graph.edges.len()
    ];
    for (_, ideal_y, index, mut rect) in drafts {
        let mut y = ideal_y;
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 10_000 {
                break;
            }
            rect.y = y;
            let mut blocked_at = None;
            for other in &placed {
                if rects_conflict(rect, *other, 14.0) {
                    let bottom = other.y + other.h + 14.0;
                    blocked_at = Some(blocked_at.map_or(bottom, |value: f32| value.max(bottom)));
                }
            }
            for other in &boxes {
                if rects_conflict(rect, *other, 0.2) {
                    let bottom = other.y + other.h + 0.2;
                    blocked_at = Some(blocked_at.map_or(bottom, |value: f32| value.max(bottom)));
                }
            }
            let Some(bottom) = blocked_at else {
                break;
            };
            let next = snap(bottom);
            if next <= y {
                y = snap(y + 14.0);
            } else {
                y = next;
            }
        }
        rect.y = y;
        placed.push(rect);
        rects[index] = rect;
    }
    rects
}

fn rects_conflict(left: Rect, right: Rect, gap: f32) -> bool {
    let x = left.x.max(right.x);
    let far_x = (left.x + left.w).min(right.x + right.w);
    if far_x - x <= 0.2 {
        return false;
    }
    left.y < right.y + right.h + gap && right.y < left.y + left.h + gap
}

pub(crate) fn edge_label_lines(edge: &Edge) -> Vec<String> {
    let columns = edge.columns.join(",");
    let target = edge.target_columns.join(",");
    let mut line = if columns.eq_ignore_ascii_case(&target) {
        columns.clone()
    } else {
        format!("{columns} → {target}")
    };
    if let Some(condition) = &edge.condition {
        line = format!("{line} ({condition})");
    }
    if matches!(edge.source, EdgeSource::ForeignKey { .. }) {
        line = format!("FK {line}");
    }
    let mut lines = vec![line];
    if let Some(label) = &edge.label {
        lines.push(label.clone());
    }
    lines
}

fn node_lines(node: &Node) -> Vec<String> {
    let mut lines = vec![node.title.clone()];
    match &node.kind {
        NodeKind::Missing => lines.push("(not pulled)".to_string()),
        NodeKind::Group { members } => lines.push(format!("{} tables", members.len())),
        NodeKind::Pulled => {
            if let Some(comment) = &node.comment {
                lines.push(clip(comment, 40));
            }
            if !node.primary_key.is_empty() {
                lines.push(clip(&format!("PK: {}", node.primary_key.join(", ")), 40));
            }
        }
    }
    lines
}

fn clip(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        text.to_string()
    } else {
        let mut shortened: String = chars.into_iter().take(max - 1).collect();
        shortened.push('…');
        shortened
    }
}

fn snap(value: f32) -> f32 {
    (value * 10.0).round() / 10.0
}

fn wide_char(ch: char) -> bool {
    matches!(ch,
        '\u{1100}'..='\u{11FF}'
            | '\u{3130}'..='\u{318F}'
            | '\u{AC00}'..='\u{D7A3}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{FF00}'..='\u{FFEF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontology::graph::{Edge, EdgeSource, Graph, Node, NodeKind, TableKey};
    use crate::ontology::schema::{RelationKind, RelationStatus};

    fn key(name: &str) -> TableKey {
        TableKey::new("main", name)
    }

    fn node(name: &str) -> Node {
        let key = key(name);
        Node {
            title: key.title(),
            comment: Some("site".to_string()),
            primary_key: vec!["ID".to_string()],
            kind: NodeKind::Pulled,
            key,
        }
    }

    fn edge(from: &str, to: &str, column: &str, status: RelationStatus) -> Edge {
        Edge {
            from: key(from),
            to: key(to),
            columns: vec![column.to_string()],
            target_columns: vec![column.to_string()],
            source: EdgeSource::Relation { status },
            kind: RelationKind::Reference,
            condition: None,
            label: Some(format!("{from}-{to}")),
        }
    }

    fn scale() -> Graph {
        let names = ["A", "B", "C", "D", "E", "F", "G", "H", "I"];
        let mut graph = Graph::default();
        for name in names {
            let node = node(name);
            graph.nodes.insert(node.key.clone(), node);
        }
        for pair in names.windows(2) {
            graph
                .edges
                .push(edge(pair[0], pair[1], "K", RelationStatus::Confirmed));
        }
        for (from, to) in [
            ("A", "C"),
            ("A", "D"),
            ("B", "E"),
            ("C", "F"),
            ("D", "G"),
            ("E", "H"),
            ("F", "I"),
            ("B", "G"),
            ("C", "H"),
            ("D", "I"),
            ("A", "E"),
            ("B", "F"),
        ] {
            graph
                .edges
                .push(edge(from, to, "E", RelationStatus::Confirmed));
        }
        for (from, to) in [("A", "B"), ("C", "D"), ("E", "F")] {
            graph
                .edges
                .push(edge(from, to, "P", RelationStatus::Confirmed));
        }
        graph
            .edges
            .push(edge("I", "I", "S", RelationStatus::Confirmed));
        graph
            .edges
            .push(edge("I", "A", "Y", RelationStatus::Confirmed));
        graph
            .edges
            .push(edge("B", "D", "D1", RelationStatus::Draft));
        graph
            .edges
            .push(edge("C", "E", "D2", RelationStatus::Draft));
        graph
            .edges
            .push(edge("D", "F", "D3", RelationStatus::Draft));
        assert_eq!(graph.nodes.len(), 9);
        graph
    }

    fn overlap(left: Rect, right: Rect) -> bool {
        let x = left.x.max(right.x);
        let y = left.y.max(right.y);
        let far_x = (left.x + left.w).min(right.x + right.w);
        let far_y = (left.y + left.h).min(right.y + right.h);
        far_x - x > 0.2 && far_y - y > 0.2
    }

    fn hits_interior(start: (f32, f32), end: (f32, f32), rect: Rect) -> bool {
        let x0 = rect.x + 0.3;
        let y0 = rect.y + 0.3;
        let x1 = rect.x + rect.w - 0.3;
        let y1 = rect.y + rect.h - 0.3;
        if x1 <= x0 || y1 <= y0 {
            return false;
        }
        let min_x = start.0.min(end.0);
        let max_x = start.0.max(end.0);
        let min_y = start.1.min(end.1);
        let max_y = start.1.max(end.1);
        max_x > x0 && min_x < x1 && max_y > y0 && min_y < y1
    }

    #[test]
    fn layout_boxes_do_not_overlap() {
        let placed = layout(&scale());
        for (left_index, left) in placed.nodes.iter().enumerate() {
            for right in placed.nodes.iter().skip(left_index + 1) {
                assert!(
                    !overlap(left.rect, right.rect),
                    "{} overlaps {}",
                    left.key.title(),
                    right.key.title()
                );
            }
        }
    }

    #[test]
    fn layout_edges_do_not_cross_box_interiors() {
        let graph = scale();
        let placed = layout(&graph);
        for edge in &placed.edges {
            let ends = &graph.edges[edge.index];
            for pair in edge.points.windows(2) {
                for node in &placed.nodes {
                    if node.key == ends.from || node.key == ends.to {
                        continue;
                    }
                    assert!(
                        !hits_interior(pair[0], pair[1], node.rect),
                        "edge {} crosses {}",
                        edge.index,
                        node.key.title()
                    );
                }
            }
        }
    }

    #[test]
    fn layout_labels_do_not_overlap_each_other_or_boxes() {
        let placed = layout(&scale());
        for (left_index, left) in placed.edges.iter().enumerate() {
            for node in &placed.nodes {
                assert!(
                    !overlap(left.label_rect, node.rect),
                    "label {} overlaps {}",
                    left.index,
                    node.key.title()
                );
            }
            for right in placed.edges.iter().skip(left_index + 1) {
                assert!(
                    !overlap(left.label_rect, right.label_rect),
                    "labels {} and {} overlap",
                    left.index,
                    right.index
                );
            }
        }
    }

    #[test]
    fn layout_parallel_edges_get_distinct_ports() {
        let graph = scale();
        let placed = layout(&graph);
        let node = placed
            .nodes
            .iter()
            .find(|node| node.key.table == "A")
            .unwrap();
        let right = snap(node.rect.x + node.rect.w);
        let mut ports = Vec::new();
        for edge in &placed.edges {
            if let Some(point) = edge
                .points
                .iter()
                .find(|point| (point.0 - right).abs() < 0.05)
            {
                ports.push(point.1);
            }
        }
        assert!(ports.len() >= 2, "{ports:?}");
        let step = node.rect.h / (ports.len() as f32 + 1.0);
        for (index, port) in ports.iter().enumerate() {
            let expected = snap(node.rect.y + step * (index as f32 + 1.0));
            assert!(
                (port - expected).abs() < 0.2,
                "port {index} at {port} expected {expected} step {step}"
            );
        }
        for pair in ports.windows(2) {
            assert!((pair[1] - pair[0] - step).abs() < 0.2);
        }
    }

    #[test]
    fn layout_handles_cycle_and_self_loop() {
        let graph = scale();
        let placed = layout(&graph);
        let loop_edge = placed.edges.iter().find(|edge| {
            let ends = &graph.edges[edge.index];
            ends.from.table == "I" && ends.to.table == "I"
        });
        let cycle = placed.edges.iter().find(|edge| {
            let ends = &graph.edges[edge.index];
            ends.from.table == "I" && ends.to.table == "A"
        });
        let loop_edge = loop_edge.unwrap();
        assert!(loop_edge.points.len() >= 4);
        let node = placed
            .nodes
            .iter()
            .find(|node| node.key.table == "I")
            .unwrap();
        let bow = loop_edge
            .points
            .iter()
            .map(|point| point.0)
            .fold(node.rect.x + node.rect.w, f32::max);
        assert!(
            (bow - (node.rect.x + node.rect.w) - 28.0).abs() < 0.2,
            "self-loop bow {bow}"
        );
        assert!(cycle.unwrap().points.len() >= 2);
    }

    #[test]
    fn layout_text_width_counts_hangul_wider_than_ascii() {
        assert!(text_width("가", 12.0) > text_width("A", 12.0));
        assert!((text_width("A", 12.0) - 7.2).abs() < 0.01);
        assert!((text_width("가", 12.0) - 12.0).abs() < 0.01);
    }

    #[test]
    fn layout_random_labels_stay_clear_of_labels_boxes_and_edges() {
        let mut label_label = 0usize;
        let mut label_box = 0usize;
        let mut crossed = 0usize;
        let mut collinear = 0usize;
        let graphs = std::iter::once(seed10_repro()).chain((0..80).map(random_graph));
        for (seed, graph) in graphs.enumerate() {
            let placed = layout(&graph);
            let (labels, boxes, crosses) = overlap_counts(&placed);
            let shared = collinear_without_shared_endpoint(&graph, &placed);
            label_label += labels;
            label_box += boxes;
            crossed += crosses;
            collinear += shared;
            assert_eq!(
                (labels, boxes, crosses, shared),
                (0, 0, 0, 0),
                "seed {seed} label-label {labels} label-box {boxes} crossings {crosses} collinear {shared}"
            );
        }
        assert_eq!((label_label, label_box, crossed, collinear), (0, 0, 0, 0));
    }

    fn collinear_without_shared_endpoint(graph: &Graph, placed: &Layout) -> usize {
        let mut count = 0;
        for left_index in 0..placed.edges.len() {
            for right_index in (left_index + 1)..placed.edges.len() {
                let left = &placed.edges[left_index];
                let right = &placed.edges[right_index];
                let left_ends = &graph.edges[left.index];
                let right_ends = &graph.edges[right.index];
                if shares_endpoint(left_ends, right_ends) {
                    continue;
                }
                for left_seg in left.points.windows(2) {
                    for right_seg in right.points.windows(2) {
                        if collinear_overlap(left_seg[0], left_seg[1], right_seg[0], right_seg[1]) {
                            count += 1;
                        }
                    }
                }
            }
        }
        count
    }

    fn shares_endpoint(left: &Edge, right: &Edge) -> bool {
        left.from == right.from
            || left.from == right.to
            || left.to == right.from
            || left.to == right.to
    }

    fn collinear_overlap(
        start: (f32, f32),
        end: (f32, f32),
        other: (f32, f32),
        last: (f32, f32),
    ) -> bool {
        if (start.0 - end.0).abs() < 0.05
            && (other.0 - last.0).abs() < 0.05
            && (start.0 - other.0).abs() < 1.0
        {
            let low = start.1.min(end.1).max(other.1.min(last.1));
            let high = start.1.max(end.1).min(other.1.max(last.1));
            return high - low > 2.0;
        }
        if (start.1 - end.1).abs() < 0.05
            && (other.1 - last.1).abs() < 0.05
            && (start.1 - other.1).abs() < 1.0
        {
            let low = start.0.min(end.0).max(other.0.min(last.0));
            let high = start.0.max(end.0).min(other.0.max(last.0));
            return high - low > 2.0;
        }
        false
    }

    fn overlap_counts(placed: &Layout) -> (usize, usize, usize) {
        let mut label_label = 0;
        let mut label_box = 0;
        let mut crossed = 0;
        for (left_index, left) in placed.edges.iter().enumerate() {
            for node in &placed.nodes {
                if overlap(left.label_rect, node.rect) {
                    label_box += 1;
                }
            }
            for right in placed.edges.iter().skip(left_index + 1) {
                if overlap(left.label_rect, right.label_rect) {
                    label_label += 1;
                }
            }
        }
        for edge in &placed.edges {
            for label in &placed.edges {
                for pair in edge.points.windows(2) {
                    if segment_crosses_rect(pair[0], pair[1], label.label_rect) {
                        crossed += 1;
                    }
                }
            }
        }
        (label_label, label_box, crossed)
    }

    fn segment_crosses_rect(start: (f32, f32), end: (f32, f32), rect: Rect) -> bool {
        let x0 = rect.x + 0.51;
        let y0 = rect.y + 0.51;
        let x1 = rect.x + rect.w - 0.51;
        let y1 = rect.y + rect.h - 0.51;
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

    fn seed10_repro() -> Graph {
        let mut graph = Graph::default();
        for index in 0..12 {
            let name = format!("T{index:02}");
            let comment = match index {
                1 | 3 | 10 => Some(format!("설명 {name}")),
                2 | 5 | 11 => Some("comment <&> \"q\"".to_string()),
                _ => None,
            };
            let key = key(&name);
            graph.nodes.insert(
                key.clone(),
                Node {
                    title: key.title(),
                    comment,
                    primary_key: vec!["ID".to_string()],
                    kind: NodeKind::Pulled,
                    key,
                },
            );
        }
        graph.edges.push(Edge {
            from: key("T02"),
            to: key("T00"),
            columns: vec!["LONG_COLUMN_NAME_ONE".to_string(), "ID".to_string()],
            target_columns: vec!["LONG_COLUMN_NAME_ONE".to_string(), "ID".to_string()],
            source: EdgeSource::Relation {
                status: RelationStatus::Draft,
            },
            kind: RelationKind::Reference,
            condition: Some("GROUP_CD='KXXXX'".to_string()),
            label: None,
        });
        graph.edges.push(Edge {
            from: key("T06"),
            to: key("T07"),
            columns: vec!["KIND_CD".to_string()],
            target_columns: vec!["KIND_CD".to_string()],
            source: EdgeSource::Relation {
                status: RelationStatus::Draft,
            },
            kind: RelationKind::Reference,
            condition: None,
            label: None,
        });
        graph.edges.push(Edge {
            from: key("T06"),
            to: key("T06"),
            columns: vec!["GROUP_CD".to_string(), "KIND_CD".to_string()],
            target_columns: vec!["GROUP_CD".to_string(), "KIND_CD".to_string()],
            source: EdgeSource::Relation {
                status: RelationStatus::Draft,
            },
            kind: RelationKind::Rollup,
            condition: None,
            label: Some("합계 = 전 방향 SUM".to_string()),
        });
        graph
    }

    fn random_graph(seed: u64) -> Graph {
        let mut rng = Rng(seed.wrapping_add(1));
        let tables = rng.below(10) + 3;
        let relations = rng.below(28) + 3;
        let columns = [
            "ID",
            "SITE_NO",
            "ARM_NO",
            "KIND_CD",
            "GROUP_CD",
            "LONG_COLUMN_NAME_ONE",
            "OBS_TM",
            "X",
        ];
        let mut graph = Graph::default();
        for index in 0..tables {
            let name = format!("T{index:02}");
            let key = key(&name);
            let comment = if rng.below(2) == 0 {
                Some(format!("설명 {name}"))
            } else {
                None
            };
            graph.nodes.insert(
                key.clone(),
                Node {
                    title: key.title(),
                    comment,
                    primary_key: vec!["ID".to_string()],
                    kind: NodeKind::Pulled,
                    key,
                },
            );
        }
        let ghost = key("GHOST");
        graph.nodes.insert(
            ghost.clone(),
            Node {
                title: ghost.title(),
                comment: None,
                primary_key: Vec::new(),
                kind: NodeKind::Missing,
                key: ghost,
            },
        );
        for _ in 0..relations {
            let from = format!("T{:02}", rng.below(tables));
            let to = if rng.below(8) == 0 {
                "GHOST".to_string()
            } else {
                format!("T{:02}", rng.below(tables))
            };
            let width = rng.below(2) + 1;
            let mut columns_from = Vec::new();
            let mut columns_to = Vec::new();
            for _ in 0..width {
                columns_from.push(columns[rng.below(columns.len())].to_string());
                let target = if rng.below(2) == 0 {
                    columns_from.last().unwrap().clone()
                } else {
                    columns[rng.below(columns.len())].to_string()
                };
                columns_to.push(target);
            }
            let rollup = rng.below(5) == 0;
            graph.edges.push(Edge {
                from: key(&from),
                to: key(&to),
                columns: columns_from,
                target_columns: columns_to,
                source: EdgeSource::Relation {
                    status: if rng.below(2) == 0 {
                        RelationStatus::Draft
                    } else {
                        RelationStatus::Confirmed
                    },
                },
                kind: if rollup {
                    RelationKind::Rollup
                } else {
                    RelationKind::Reference
                },
                condition: if rng.below(3) == 0 {
                    Some(format!("GROUP_CD='K{}'", "X".repeat(rng.below(21))))
                } else {
                    None
                },
                label: if rollup {
                    Some("합계 = 전 방향 SUM".to_string())
                } else {
                    None
                },
            });
        }
        graph
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, limit: usize) -> usize {
            (self.next() % limit as u64) as usize
        }
    }
}
