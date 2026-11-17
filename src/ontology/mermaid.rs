//! Optional Mermaid text. Node ids follow sorted table order.

use crate::ontology::graph::{EdgeSource, Graph};
use crate::ontology::layout::edge_label_lines;
use crate::ontology::schema::RelationStatus;

pub fn to_mermaid(graph: &Graph) -> String {
    let mut keys: Vec<_> = graph.nodes.keys().cloned().collect();
    keys.sort();
    let mut out = String::from("flowchart LR\n");
    for (index, key) in keys.iter().enumerate() {
        let title = graph.nodes[key].title.replace('"', "#quot;");
        out.push_str(&format!("n{index}[\"{title}\"]\n"));
    }
    for edge in &graph.edges {
        let from = keys.iter().position(|key| key == &edge.from).unwrap_or(0);
        let to = keys.iter().position(|key| key == &edge.to).unwrap_or(0);
        let label = edge_label_lines(edge)
            .join("<br>")
            .replace('"', "#quot;")
            .replace('\n', "<br>");
        let arrow = match (&edge.source, edge.kind) {
            (
                EdgeSource::Relation {
                    status: RelationStatus::Draft,
                },
                _,
            ) => "-.->",
            (_, crate::ontology::schema::RelationKind::Rollup) => "==>",
            _ => "-->",
        };
        out.push_str(&format!("n{from} {arrow}|\"{label}\"| n{to}\n"));
    }
    out
}
