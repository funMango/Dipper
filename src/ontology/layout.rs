//! Deterministic left-to-right layout. Coordinates are rounded to 0.1 px.

use std::collections::{BTreeMap, BTreeSet};

use crate::ontology::graph::{Edge, EdgeSource, Graph, Node, NodeKind, TableKey};

mod channel;
#[cfg(test)]
mod check;
mod route;

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
    pub legend: Rect,
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
    for (index, edge) in graph.edges.iter().enumerate() {
        if edge.from == edge.to {
            continue;
        }
        let (left, right) = span(index, edge, &index_of, &forward, &layer);
        if right > left + 1 {
            let mut step = left + 1;
            while step < right {
                slots[step].push(Slot {
                    name: format!("~{index}-{step}"),
                    node: None,
                    edge: Some(index),
                    width: 0.0,
                    height: 8.0,
                });
                step += 1;
            }
        }
    }
    for _ in 0..4 {
        order_layer(&mut slots, graph, &index_of, &forward, true);
        order_layer(&mut slots, graph, &index_of, &forward, false);
    }

    let label_lines: Vec<Vec<String>> = graph.edges.iter().map(edge_label_lines).collect();
    let label_sizes: Vec<(f32, f32)> = label_lines
        .iter()
        .map(|lines| {
            let width = lines
                .iter()
                .map(|line| text_width(line, 12.0))
                .fold(0.0, f32::max)
                + 8.0;
            let height = lines.len() as f32 * 16.0;
            (snap(width), snap(height))
        })
        .collect();
    let provisional = pack_columns(&slots);
    let orders = port_orders(graph, &layer, &index_of, &slots, &provisional);
    grow_for_ports(&mut slots, &orders);
    let placed_slots = pack_columns(&slots);
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
    let ports = materialize_ports(&orders, &keys, &node_rect);
    let mut gap_count = slots.len().saturating_sub(1);
    if graph.edges.iter().any(|edge| {
        edge.from == edge.to
            && layer
                .get(index_of.get(&edge.from).copied().unwrap_or(0))
                .copied()
                .unwrap_or(0)
                + 1
                == slots.len()
    }) {
        gap_count = slots.len();
    }
    let mut nets = vec![Vec::new(); gap_count];
    for (index, edge) in graph.edges.iter().enumerate() {
        if edge.from == edge.to {
            let node = index_of[&edge.from];
            let gap = layer[node];
            if gap >= nets.len() {
                continue;
            }
            let (port_a, port_b) = self_ports(&ports, node, index);
            nets[gap].push(channel::Net {
                edge: channel::EdgeId(index),
                kind: channel::NetKind::SelfLoop {
                    port_b,
                    label_w: label_sizes[index].0,
                    label_h: label_sizes[index].1,
                },
                y_in: port_a,
                y_out: port_b,
                travel_y: port_a,
                return_y: None,
            });
            continue;
        }
        let from_layer = layer[index_of[&edge.from]];
        let to_layer = layer[index_of[&edge.to]];
        let (left_node, right_node, left_layer, right_layer) = if from_layer <= to_layer {
            (
                index_of[&edge.from],
                index_of[&edge.to],
                from_layer,
                to_layer,
            )
        } else {
            (
                index_of[&edge.to],
                index_of[&edge.from],
                to_layer,
                from_layer,
            )
        };
        let source_y = ports.y(left_node, 1, index, 0);
        let target_y = ports.y(right_node, 0, index, 0);
        for gap in left_layer..right_layer {
            if gap >= nets.len() {
                break;
            }
            let y_in = if gap == left_layer {
                source_y
            } else {
                dummy_center(&slots, &placed_slots, index, gap)
            };
            let y_out = if gap + 1 == right_layer {
                target_y
            } else {
                dummy_center(&slots, &placed_slots, index, gap + 1)
            };
            let kind = if gap == left_layer {
                channel::NetKind::First {
                    label_w: label_sizes[index].0,
                    label_h: label_sizes[index].1,
                }
            } else {
                channel::NetKind::Through
            };
            let travel_y = y_in;
            nets[gap].push(channel::Net {
                edge: channel::EdgeId(index),
                kind,
                y_in,
                y_out,
                travel_y,
                return_y: None,
            });
        }
    }
    let plans: Vec<channel::ChannelPlan> = nets
        .iter_mut()
        .map(|gap_nets| channel::plan_channel(gap_nets))
        .collect();
    let mut layer_w = vec![0.0f32; slots.len()];
    for (index, column) in slots.iter().enumerate() {
        layer_w[index] = column.iter().map(|slot| slot.width).fold(0.0, f32::max);
    }
    let mut gap_w = vec![96.0f32; gap_count.max(slots.len().saturating_sub(1))];
    for (index, plan) in plans.iter().enumerate() {
        if index >= gap_w.len() {
            break;
        }
        let width = channel::ZONE_PAD
            + channel::TRACK * plan.n_left as f32
            + channel::ZONE_PAD
            + plan.max_label_w
            + channel::ZONE_PAD
            + channel::TRACK * plan.n_right as f32
            + 12.0;
        gap_w[index] = snap(width.max(96.0));
    }
    let mut layer_x = vec![16.0f32; slots.len()];
    for index in 1..slots.len() {
        let gap = gap_w.get(index - 1).copied().unwrap_or(96.0);
        layer_x[index] = snap(layer_x[index - 1] + layer_w[index - 1] + gap);
    }
    for (layer_index, column) in slots.iter().enumerate() {
        for (slot_index, slot) in column.iter().enumerate() {
            if let Some(node_index) = slot.node {
                let key = &keys[node_index];
                if let Some(rect) = node_rect.get_mut(key) {
                    rect.x = snap(layer_x[layer_index]);
                    rect.y = placed_slots[layer_index][slot_index].y;
                    rect.w = placed_slots[layer_index][slot_index].w;
                    rect.h = placed_slots[layer_index][slot_index].h;
                }
            }
        }
    }
    let geometries: Vec<route::GapGeometry> = (0..plans.len())
        .map(|gap| {
            let gap_left = if gap < slots.len() {
                snap(layer_x[gap] + layer_w[gap])
            } else {
                snap(layer_x[slots.len() - 1] + layer_w[slots.len() - 1])
            };
            let plan = &plans[gap];
            let label_x = snap(
                gap_left
                    + channel::ZONE_PAD
                    + channel::TRACK * plan.n_left as f32
                    + channel::ZONE_PAD,
            );
            let right_zone_x = snap(label_x + plan.max_label_w + channel::ZONE_PAD);
            let gap_right = snap(gap_left + gap_w.get(gap).copied().unwrap_or(96.0));
            route::GapGeometry {
                gap_left,
                left_zone_x: gap_left,
                label_x,
                right_zone_x,
                gap_right,
            }
        })
        .collect();
    let mut placed_edges = Vec::new();
    for (index, edge) in graph.edges.iter().enumerate() {
        let points = if edge.from == edge.to {
            let node = index_of[&edge.from];
            let gap = layer[node];
            let net = nets
                .get(gap)
                .and_then(|list| list.iter().find(|net| net.edge.0 == index));
            if let Some(net) = net {
                let left = track_pair(&plans[gap].left_tracks, index);
                let right = plans[gap]
                    .right_tracks
                    .iter()
                    .find(|(edge_id, _)| edge_id.0 == index)
                    .map(|(_, track)| *track);
                route::self_loop(node_rect[&keys[node]], &geometries[gap], net, left, right)
            } else {
                Vec::new()
            }
        } else {
            let from_layer = layer[index_of[&edge.from]];
            let to_layer = layer[index_of[&edge.to]];
            let (left_node, right_node, left_layer, right_layer) = if from_layer <= to_layer {
                (
                    index_of[&edge.from],
                    index_of[&edge.to],
                    from_layer,
                    to_layer,
                )
            } else {
                (
                    index_of[&edge.to],
                    index_of[&edge.from],
                    to_layer,
                    from_layer,
                )
            };
            let mut legs = Vec::new();
            for gap in left_layer..right_layer {
                if gap >= nets.len() {
                    break;
                }
                if let Some(net) = nets[gap].iter().find(|net| net.edge.0 == index) {
                    let left = plans[gap]
                        .left_tracks
                        .iter()
                        .find(|(edge_id, _)| edge_id.0 == index)
                        .map(|(_, track)| *track);
                    let right = plans[gap]
                        .right_tracks
                        .iter()
                        .find(|(edge_id, _)| edge_id.0 == index)
                        .map(|(_, track)| *track);
                    legs.push((geometries[gap], net, left, right));
                }
            }
            let start = (
                snap(node_rect[&keys[left_node]].x + node_rect[&keys[left_node]].w),
                ports.y(left_node, 1, index, 0),
            );
            let end = (
                snap(node_rect[&keys[right_node]].x),
                ports.y(right_node, 0, index, 0),
            );
            let mut points = route::polyline(start, end, &legs);
            if from_layer > to_layer {
                points.reverse();
            }
            points
        };
        let label_rect = plans.iter().enumerate().find_map(|(gap, plan)| {
            plan.label_rects.iter().find_map(|(edge_id, rect)| {
                (edge_id.0 == index).then(|| {
                    let mut placed = *rect;
                    placed.x = geometries[gap].label_x;
                    placed
                })
            })
        });
        placed_edges.push(PlacedEdge {
            index,
            points,
            label_rect: label_rect.unwrap_or(Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            }),
            label_lines: label_lines[index].clone(),
        });
    }
    let mut content_bottom = 40.0f32;
    let mut content_right = 32.0f32;
    for rect in node_rect.values() {
        content_bottom = content_bottom.max(rect.y + rect.h);
        content_right = content_right.max(rect.x + rect.w);
    }
    for edge in &placed_edges {
        content_bottom = content_bottom.max(edge.label_rect.y + edge.label_rect.h);
        content_right = content_right.max(edge.label_rect.x + edge.label_rect.w);
        for point in &edge.points {
            content_bottom = content_bottom.max(point.1);
            content_right = content_right.max(point.0);
        }
    }
    let legend = Rect {
        x: 16.0,
        y: snap(content_bottom + 24.0),
        w: snap(legend_width()),
        h: 16.0,
    };
    Layout {
        width: snap(content_right.max(legend.x + legend.w + 16.0) + 16.0),
        height: snap(legend.y + legend.h + 16.0),
        nodes: keys
            .into_iter()
            .map(|key| PlacedNode {
                layer: node_layer[&key],
                rect: node_rect[&key],
                key,
            })
            .collect(),
        edges: placed_edges,
        legend,
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

fn pack_columns(slots: &[Vec<Slot>]) -> Vec<Vec<Rect>> {
    let mut placed = Vec::new();
    let mut heights = Vec::new();
    for column in slots {
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
        heights.push(y);
        placed.push(rects);
    }
    let tallest = heights.iter().copied().fold(40.0, f32::max);
    for (column, rects) in slots.iter().zip(placed.iter_mut()) {
        let bottom = rects.last().map(|rect| rect.y + rect.h).unwrap_or(40.0);
        let shift = snap(((tallest - 40.0) - (bottom - 40.0)).max(0.0) / 2.0);
        for rect in rects.iter_mut() {
            rect.y = snap(rect.y + shift);
        }
        let _ = column;
    }
    placed
}

#[derive(Clone)]
struct PortEnd {
    edge: usize,
    ord: u8,
    opposite: f32,
}

struct PortOrders {
    left: Vec<Vec<PortEnd>>,
    right: Vec<Vec<PortEnd>>,
}

struct PortYs {
    left: Vec<Vec<f32>>,
    right: Vec<Vec<f32>>,
    left_edge: Vec<Vec<(usize, u8)>>,
    right_edge: Vec<Vec<(usize, u8)>>,
}

impl PortYs {
    fn y(&self, node: usize, side: u8, edge: usize, ord: u8) -> f32 {
        let (edges, ys) = if side == 0 {
            (&self.left_edge[node], &self.left[node])
        } else {
            (&self.right_edge[node], &self.right[node])
        };
        edges
            .iter()
            .position(|(found, found_ord)| *found == edge && *found_ord == ord)
            .map(|index| ys[index])
            .unwrap_or(0.0)
    }
}

fn port_orders(
    graph: &Graph,
    layer: &[usize],
    index_of: &BTreeMap<TableKey, usize>,
    slots: &[Vec<Slot>],
    placed: &[Vec<Rect>],
) -> PortOrders {
    let mut orders = PortOrders {
        left: vec![Vec::new(); layer.len()],
        right: vec![Vec::new(); layer.len()],
    };
    for (index, edge) in graph.edges.iter().enumerate() {
        if edge.from == edge.to {
            let node = index_of[&edge.from];
            let center = node_center(node, layer, slots, placed);
            orders.right[node].push(PortEnd {
                edge: index,
                ord: 0,
                opposite: center,
            });
            orders.right[node].push(PortEnd {
                edge: index,
                ord: 1,
                opposite: center,
            });
            continue;
        }
        let from = index_of[&edge.from];
        let to = index_of[&edge.to];
        let (left_node, right_node, left_layer, right_layer) = if layer[from] <= layer[to] {
            (from, to, layer[from], layer[to])
        } else {
            (to, from, layer[to], layer[from])
        };
        let next_y = if right_layer > left_layer + 1 {
            dummy_center(slots, placed, index, left_layer + 1)
        } else {
            node_center(right_node, layer, slots, placed)
        };
        let prev_y = if right_layer > left_layer + 1 {
            dummy_center(slots, placed, index, right_layer - 1)
        } else {
            node_center(left_node, layer, slots, placed)
        };
        orders.right[left_node].push(PortEnd {
            edge: index,
            ord: 0,
            opposite: next_y,
        });
        orders.left[right_node].push(PortEnd {
            edge: index,
            ord: 0,
            opposite: prev_y,
        });
    }
    for ends in orders.left.iter_mut().chain(orders.right.iter_mut()) {
        ends.sort_by(|left, right| {
            left.opposite
                .total_cmp(&right.opposite)
                .then(left.edge.cmp(&right.edge))
                .then(left.ord.cmp(&right.ord))
        });
    }
    orders
}

fn node_center(node: usize, layer: &[usize], slots: &[Vec<Slot>], placed: &[Vec<Rect>]) -> f32 {
    let layer_index = layer[node];
    slots[layer_index]
        .iter()
        .zip(&placed[layer_index])
        .find(|(slot, _)| slot.node == Some(node))
        .map(|(_, rect)| snap(rect.y + rect.h / 2.0))
        .unwrap_or(40.0)
}

fn dummy_center(slots: &[Vec<Slot>], placed: &[Vec<Rect>], edge: usize, layer_index: usize) -> f32 {
    slots
        .get(layer_index)
        .zip(placed.get(layer_index))
        .and_then(|(column, rects)| {
            column.iter().zip(rects).find_map(|(slot, rect)| {
                (slot.edge == Some(edge)).then_some(snap(rect.y + rect.h / 2.0))
            })
        })
        .unwrap_or_else(|| snap(40.0 + edge as f32))
}

fn grow_for_ports(slots: &mut [Vec<Slot>], orders: &PortOrders) {
    for column in slots.iter_mut() {
        for slot in column.iter_mut() {
            let Some(node) = slot.node else {
                continue;
            };
            let count = orders.left[node].len().max(orders.right[node].len());
            let needed = snap(channel::PORT_MIN * (count as f32 + 1.0));
            if needed > slot.height {
                slot.height = needed;
            }
        }
    }
}

fn materialize_ports(
    orders: &PortOrders,
    keys: &[TableKey],
    node_rect: &BTreeMap<TableKey, Rect>,
) -> PortYs {
    let mut ports = PortYs {
        left: Vec::new(),
        right: Vec::new(),
        left_edge: Vec::new(),
        right_edge: Vec::new(),
    };
    for node in 0..orders.left.len() {
        let rect = node_rect[&keys[node]];
        ports.left.push(spread(rect, orders.left[node].len()));
        ports.right.push(spread(rect, orders.right[node].len()));
        ports.left_edge.push(
            orders.left[node]
                .iter()
                .map(|end| (end.edge, end.ord))
                .collect(),
        );
        ports.right_edge.push(
            orders.right[node]
                .iter()
                .map(|end| (end.edge, end.ord))
                .collect(),
        );
    }
    ports
}

fn spread(rect: Rect, count: usize) -> Vec<f32> {
    if count == 0 {
        return Vec::new();
    }
    let n = count as f32;
    (0..count)
        .map(|index| snap(rect.y + rect.h * (index as f32 + 1.0) / (n + 1.0)))
        .collect()
}

fn self_ports(ports: &PortYs, node: usize, edge: usize) -> (f32, f32) {
    let first = ports.y(node, 1, edge, 0);
    let second = ports.y(node, 1, edge, 1);
    if first <= second {
        (first, second)
    } else {
        (second, first)
    }
}

fn track_pair(tracks: &[(channel::EdgeId, usize)], edge: usize) -> (Option<usize>, Option<usize>) {
    let found: Vec<usize> = tracks
        .iter()
        .filter(|(edge_id, _)| edge_id.0 == edge)
        .map(|(_, track)| *track)
        .collect();
    (found.first().copied(), found.get(1).copied())
}

fn legend_width() -> f32 {
    text_width("confirmed", 12.0)
        + text_width("draft", 12.0)
        + text_width("FK", 12.0)
        + text_width("rollup", 12.0)
        + 220.0
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
            for point in &edge.points {
                if (point.0 - right).abs() < 0.05 {
                    ports.push(point.1);
                }
            }
        }
        ports.sort_by(|left, right| left.total_cmp(right));
        ports.dedup_by(|left, right| (*left - *right).abs() < 0.05);
        assert!(ports.len() >= 2, "{ports:?}");
        for pair in ports.windows(2) {
            assert!(pair[1] - pair[0] + 0.05 >= 6.0, "{ports:?}");
        }
        let loop_node = placed
            .nodes
            .iter()
            .find(|node| node.key.table == "I")
            .unwrap();
        let loop_right = snap(loop_node.rect.x + loop_node.rect.w);
        let mut loop_ports = Vec::new();
        for edge in &placed.edges {
            let ends = &graph.edges[edge.index];
            if ends.from.table == "I" && ends.to.table == "I" {
                for point in &edge.points {
                    if (point.0 - loop_right).abs() < 0.05 {
                        loop_ports.push(point.1);
                    }
                }
            }
        }
        loop_ports.sort_by(|left, right| left.total_cmp(right));
        loop_ports.dedup_by(|left, right| (*left - *right).abs() < 0.05);
        assert!(loop_ports.len() >= 2, "{loop_ports:?}");
    }

    #[test]
    fn layout_handles_cycle_and_self_loop() {
        let graph = scale();
        let placed = layout(&graph);
        let loop_edge = placed
            .edges
            .iter()
            .find(|edge| {
                let ends = &graph.edges[edge.index];
                ends.from.table == "I" && ends.to.table == "I"
            })
            .unwrap();
        let cycle = placed.edges.iter().find(|edge| {
            let ends = &graph.edges[edge.index];
            ends.from.table == "I" && ends.to.table == "A"
        });
        assert!(loop_edge.points.len() >= 4);
        assert!(cycle.unwrap().points.len() >= 2);
        assert!(loop_wraps_label(loop_edge));
    }

    fn loop_wraps_label(edge: &PlacedEdge) -> bool {
        let top = edge.label_rect.y;
        let bottom = edge.label_rect.y + edge.label_rect.h;
        let above = edge.points.windows(2).any(|pair| {
            (pair[0].1 - pair[1].1).abs() < 0.05
                && (pair[0].1 - (top - 2.0)).abs() < 0.6
                && pair[0].0.min(pair[1].0) <= edge.label_rect.x + 0.5
                && pair[0].0.max(pair[1].0) >= edge.label_rect.x + edge.label_rect.w - 0.5
        });
        let below = edge.points.windows(2).any(|pair| {
            (pair[0].1 - pair[1].1).abs() < 0.05
                && (pair[0].1 - (bottom + 2.0)).abs() < 0.6
                && pair[0].0.min(pair[1].0) <= edge.label_rect.x + 0.5
                && pair[0].0.max(pair[1].0) >= edge.label_rect.x + edge.label_rect.w - 0.5
        });
        above && below
    }

    #[test]
    fn layout_text_width_counts_hangul_wider_than_ascii() {
        assert!(text_width("가", 12.0) > text_width("A", 12.0));
        assert!((text_width("A", 12.0) - 7.2).abs() < 0.01);
        assert!((text_width("가", 12.0) - 12.0).abs() < 0.01);
    }

    #[test]
    fn layout_fuzz_four_checks_and_legend_are_zero() {
        let started = std::time::Instant::now();
        let mut edges = 0usize;
        let mut crossings = 0usize;
        let mut width_sum = 0.0f32;
        let mut height_sum = 0.0f32;
        for seed in 0..48u64 {
            let graph = fuzz_graph(seed);
            edges += graph.edges.len();
            let placed = layout(&graph);
            let violations = super::check::check_layout(&graph, &placed);
            crossings += violations.crossings;
            width_sum += placed.width;
            height_sum += placed.height;
            assert_eq!(
                (
                    violations.label_overlap,
                    violations.line_through_label,
                    violations.collinear_unshared,
                    violations.label_not_own_or_far,
                    violations.legend_hits,
                ),
                (0, 0, 0, 0, 0),
                "seed {seed} {violations:?}"
            );
        }
        println!(
            "fuzz seeds=48 edges={edges} c1=0 c2=0 c3=0 c4=0 c5=0 crossings={crossings} width_sum={width_sum} height_sum={height_sum} ms={}",
            started.elapsed().as_millis()
        );
    }

    #[test]
    fn layout_seed10_repro_four_checks_are_zero() {
        let graph = seed10_repro();
        let placed = layout(&graph);
        let violations = super::check::check_layout(&graph, &placed);
        assert_eq!(
            (
                violations.label_overlap,
                violations.line_through_label,
                violations.collinear_unshared,
                violations.label_not_own_or_far,
                violations.legend_hits,
            ),
            (0, 0, 0, 0, 0),
            "{violations:?}"
        );
    }

    #[test]
    fn layout_golden_fixture_within_caps() {
        let graph = demo_graph();
        let placed = layout(&graph);
        let violations = super::check::check_layout(&graph, &placed);
        assert_eq!(violations.label_overlap, 0, "{violations:?}");
        assert_eq!(violations.line_through_label, 0, "{violations:?}");
        assert_eq!(violations.collinear_unshared, 0, "{violations:?}");
        assert_eq!(violations.label_not_own_or_far, 0, "{violations:?}");
        assert_eq!(violations.legend_hits, 0, "{violations:?}");
        assert!(violations.crossings <= 2, "{}", violations.crossings);
        assert!(placed.width <= 1106.9, "{}", placed.width);
    }

    #[test]
    fn layout_self_loop_encloses_its_label() {
        let mut graph = Graph::default();
        let node = node("LOOP");
        graph.nodes.insert(node.key.clone(), node);
        graph
            .edges
            .push(edge("LOOP", "LOOP", "A", RelationStatus::Confirmed));
        graph
            .edges
            .push(edge("LOOP", "LOOP", "B", RelationStatus::Draft));
        let placed = layout(&graph);
        let loops: Vec<_> = placed
            .edges
            .iter()
            .filter(|edge| {
                let ends = &graph.edges[edge.index];
                ends.from.table == "LOOP" && ends.to.table == "LOOP"
            })
            .collect();
        assert_eq!(loops.len(), 2);
        assert!(loop_wraps_label(loops[0]));
        assert!(loop_wraps_label(loops[1]));
        assert_ne!(loops[0].label_rect.y, loops[1].label_rect.y);
    }

    #[test]
    fn layout_ports_follow_opposite_end_order() {
        let graph = scale();
        let placed = layout(&graph);
        for node in &placed.nodes {
            let right = snap(node.rect.x + node.rect.w);
            let left = snap(node.rect.x);
            for side in [right, left] {
                let mut ports = Vec::new();
                for edge in &placed.edges {
                    for point in &edge.points {
                        if (point.0 - side).abs() < 0.05
                            && point.1 >= node.rect.y - 0.5
                            && point.1 <= node.rect.y + node.rect.h + 0.5
                        {
                            ports.push(point.1);
                        }
                    }
                }
                ports.sort_by(|a, b| a.total_cmp(b));
                ports.dedup_by(|a, b| (*a - *b).abs() < 0.05);
                for pair in ports.windows(2) {
                    assert!(
                        pair[1] - pair[0] + 0.05 >= 6.0,
                        "{} {ports:?}",
                        node.key.table
                    );
                }
            }
        }
    }

    fn demo_graph() -> Graph {
        let root = std::env::temp_dir().join(format!("dipper-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let fixture =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/diagram");
        for folder in ["extracted", "overlay"] {
            copy_tree(&fixture.join(folder), &root.join("ontology").join(folder));
        }
        let docs = crate::ontology::load::load_source(&root, "demo").unwrap();
        let built =
            crate::ontology::graph::build(&docs.into_iter().map(|doc| doc.doc).collect::<Vec<_>>());
        let pattern = crate::ontology::graph::TablePattern::parse("main.FACT_A_*").unwrap();
        crate::ontology::graph::select(
            &built,
            &crate::ontology::graph::Selection {
                roots: vec![pattern],
                depth: 2,
                groups: Vec::new(),
                confirmed_only: false,
            },
        )
        .unwrap()
    }

    fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
        for entry in std::fs::read_dir(from).unwrap() {
            let path = entry.unwrap().path();
            let dest = to.join(path.file_name().unwrap());
            if path.is_dir() {
                std::fs::create_dir_all(&dest).unwrap();
                copy_tree(&path, &dest);
            } else {
                std::fs::create_dir_all(to).unwrap();
                std::fs::copy(&path, &dest).unwrap();
            }
        }
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

    fn fuzz_graph(seed: u64) -> Graph {
        let mut rng = Rng(seed.wrapping_add(0x9E37));
        let tables = 3 + (seed as usize % 10);
        let relations = 3 + (seed as usize * 37) % 198;
        let columns = [
            "ID", "SITE_NO", "ARM_NO", "KIND_CD", "GROUP_CD", "OBS_TM", "X",
        ];
        let mut graph = Graph::default();
        for index in 0..tables {
            let name = format!("T{index:02}");
            let key = key(&name);
            graph.nodes.insert(
                key.clone(),
                Node {
                    title: key.title(),
                    comment: if rng.below(2) == 0 {
                        Some(format!("note {name}"))
                    } else {
                        None
                    },
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
        graph
            .edges
            .push(edge("T00", "T00", "ID", RelationStatus::Confirmed));
        graph
            .edges
            .push(edge("T00", "T01", "ID", RelationStatus::Confirmed));
        graph
            .edges
            .push(edge("T00", "T01", "SITE_NO", RelationStatus::Draft));
        for index in 3..relations {
            let from = format!("T{:02}", rng.below(tables));
            let to = if rng.below(11) == 0 {
                "GHOST".to_string()
            } else if rng.below(7) == 0 {
                from.clone()
            } else {
                format!("T{:02}", rng.below(tables))
            };
            let column = columns[rng.below(columns.len())];
            let mut edge = edge(
                &from,
                &to,
                column,
                if rng.below(2) == 0 {
                    RelationStatus::Draft
                } else {
                    RelationStatus::Confirmed
                },
            );
            if rng.below(4) == 0 {
                edge.condition = Some(format!("GROUP_CD='K{}'", "X".repeat(rng.below(8))));
            }
            if rng.below(5) == 0 {
                edge.kind = RelationKind::Rollup;
                edge.label = Some("합계 = 전 방향 SUM".to_string());
            }
            let _ = index;
            graph.edges.push(edge);
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
