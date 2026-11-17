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
    let mut gap = vec![120.0f32; slots.len().saturating_sub(1)];
    for (index, edge) in graph.edges.iter().enumerate() {
        if edge.from == edge.to {
            continue;
        }
        let (left, _) = span(index, edge, &index_of, &forward, &layer);
        if left < gap.len() {
            gap[left] = gap[left].max(label_widths[index] + 48.0).max(120.0);
        }
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

    let mut placed_edges = Vec::new();
    for (index, edge) in graph.edges.iter().enumerate() {
        let lines = label_lines[index].clone();
        let points = if edge.from == edge.to {
            loop_points(&node_rect[&edge.from])
        } else {
            route_edge(
                index,
                edge,
                &forward,
                &slots,
                &placed_slots,
                &node_rect,
                &layer,
                &index_of,
            )
        };
        let label_rect = if edge.from == edge.to {
            let rect = node_rect[&edge.from];
            Rect {
                x: snap(rect.x + rect.w + 32.0),
                y: snap(rect.y),
                w: snap(label_widths[index]),
                h: snap(lines.len() as f32 * 16.0),
            }
        } else {
            let (left, _) = span(index, edge, &index_of, &forward, &layer);
            let gutter_left = layer_x[left] + layer_w[left];
            let gutter_right = layer_x[left + 1];
            let width = label_widths[index];
            let x = snap(gutter_left + (gutter_right - gutter_left - width).max(0.0) / 2.0);
            let y = snap(points.first().map(|point| point.1).unwrap_or(40.0));
            Rect {
                x,
                y,
                w: snap(width),
                h: snap(lines.len() as f32 * 16.0),
            }
        };
        placed_edges.push(PlacedEdge {
            index,
            points,
            label_rect,
            label_lines: lines,
        });
    }
    separate_labels(&mut placed_edges);

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
        height = height.max(edge.label_rect.y + edge.label_rect.h + 16.0);
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
    forward: &[bool],
    slots: &[Vec<Slot>],
    placed: &[Vec<Rect>],
    node_rect: &BTreeMap<TableKey, Rect>,
    layer: &[usize],
    index_of: &BTreeMap<TableKey, usize>,
) -> Vec<(f32, f32)> {
    let from_layer = layer[index_of[&edge.from]];
    let to_layer = layer[index_of[&edge.to]];
    let left_to_right = from_layer <= to_layer;
    let source = if left_to_right { &edge.from } else { &edge.to };
    let target = if left_to_right { &edge.to } else { &edge.from };
    let source_rect = node_rect[source];
    let target_rect = node_rect[target];
    let source_y = port_y(
        source, true, index, edge, slots, placed, node_rect, layer, index_of, forward,
    );
    let target_y = port_y(
        target, false, index, edge, slots, placed, node_rect, layer, index_of, forward,
    );
    let mut points = vec![(snap(source_rect.x + source_rect.w), snap(source_y))];
    let left = from_layer.min(to_layer);
    let right = from_layer.max(to_layer);
    if right == left {
        points.push((snap(target_rect.x), snap(target_y)));
    } else {
        for layer_index in left..right {
            let gutter_left = source_rect.x + source_rect.w;
            let _ = gutter_left;
            let mid = snap(
                (placed_right(slots, placed, layer_index) + placed[layer_index + 1][0].x) / 2.0,
            );
            let y = if layer_index == left {
                source_y
            } else {
                target_y
            };
            points.push((mid, snap(y)));
            if layer_index + 1 < right {
                if let Some(dummy_y) = dummy_y(index, layer_index + 1, slots, placed) {
                    points.push((snap(placed[layer_index + 1][0].x), snap(dummy_y)));
                    points.push((
                        snap(placed_right(slots, placed, layer_index + 1)),
                        snap(dummy_y),
                    ));
                }
            }
        }
        points.push((snap(target_rect.x), snap(target_y)));
    }
    if !left_to_right {
        points.reverse();
    }
    points
}

fn placed_right(slots: &[Vec<Slot>], placed: &[Vec<Rect>], layer_index: usize) -> f32 {
    slots[layer_index]
        .iter()
        .zip(&placed[layer_index])
        .map(|(slot, rect)| rect.x + slot.width.max(rect.w))
        .fold(
            placed[layer_index]
                .first()
                .map(|rect| rect.x)
                .unwrap_or(0.0),
            f32::max,
        )
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

#[allow(clippy::too_many_arguments)]
fn port_y(
    key: &TableKey,
    outgoing: bool,
    edge_index: usize,
    _edge: &Edge,
    slots: &[Vec<Slot>],
    placed: &[Vec<Rect>],
    node_rect: &BTreeMap<TableKey, Rect>,
    layer: &[usize],
    index_of: &BTreeMap<TableKey, usize>,
    forward: &[bool],
) -> f32 {
    let rect = node_rect[key];
    let node = index_of[key];
    let mut rank = 0usize;
    let mut count = 0usize;
    for (index, edge) in index_of.iter().enumerate() {
        let _ = (index, edge);
    }
    for (index, is_forward) in forward.iter().enumerate() {
        let _ = (index, is_forward, node, outgoing, slots, placed, layer);
        count += 1;
        if index == edge_index {
            rank = count;
        }
    }
    let count = count.max(1) as f32;
    snap(rect.y + rect.h * rank as f32 / (count + 1.0))
}

fn loop_points(rect: &Rect) -> Vec<(f32, f32)> {
    let y1 = snap(rect.y + rect.h * 0.35);
    let y2 = snap(rect.y + rect.h * 0.65);
    let x = snap(rect.x + rect.w);
    let bow = snap(x + 28.0);
    vec![(x, y1), (bow, y1), (bow, y2), (x, y2)]
}

fn separate_labels(edges: &mut [PlacedEdge]) {
    let mut order: Vec<usize> = (0..edges.len()).collect();
    order.sort_by(|left, right| {
        edges[*left]
            .label_rect
            .x
            .partial_cmp(&edges[*right].label_rect.x)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                edges[*left]
                    .label_rect
                    .y
                    .partial_cmp(&edges[*right].label_rect.y)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(edges[*left].index.cmp(&edges[*right].index))
    });
    let mut previous: Option<Rect> = None;
    for index in order {
        if let Some(prev) = previous {
            let same_column = (edges[index].label_rect.x - prev.x).abs() < 80.0;
            let overlap = edges[index].label_rect.y < prev.y + prev.h + 14.0;
            if same_column && overlap {
                edges[index].label_rect.y = snap(prev.y + prev.h + 14.0);
            }
        }
        previous = Some(edges[index].label_rect);
    }
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
        let mut ports = Vec::new();
        for edge in &placed.edges {
            let ends = &graph.edges[edge.index];
            if ends.from.table == "A" && ends.to.table == "B" {
                ports.push(edge.points[0].1);
            }
        }
        assert!(ports.len() >= 2);
        assert!((ports[0] - ports[1]).abs() > 0.2);
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
        assert!(loop_edge.unwrap().points.len() >= 4);
        assert!(cycle.unwrap().points.len() >= 2);
    }

    #[test]
    fn layout_text_width_counts_hangul_wider_than_ascii() {
        assert!(text_width("가", 12.0) > text_width("A", 12.0));
        assert!((text_width("A", 12.0) - 7.2).abs() < 0.01);
        assert!((text_width("가", 12.0) - 12.0).abs() < 0.01);
    }
}
