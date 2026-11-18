//! Table and relationship graph built only from ontology JSON.

use std::collections::BTreeMap;

use crate::ontology::schema::{OntologyDoc, RelationKind, RelationStatus};

#[derive(Clone, Debug)]
pub struct TableKey {
    pub owner: String,
    pub table: String,
}

impl TableKey {
    pub fn new(owner: &str, table: &str) -> Self {
        Self {
            owner: owner.to_string(),
            table: table.to_string(),
        }
    }

    fn fold(&self) -> (String, String) {
        (
            self.owner.to_ascii_uppercase(),
            self.table.to_ascii_uppercase(),
        )
    }

    pub fn title(&self) -> String {
        format!("{}.{}", self.owner, self.table)
    }
}

impl PartialEq for TableKey {
    fn eq(&self, other: &Self) -> bool {
        self.fold() == other.fold()
    }
}

impl Eq for TableKey {}

impl PartialOrd for TableKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TableKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.fold().cmp(&other.fold())
    }
}

#[derive(Clone, Debug)]
pub struct TablePattern {
    pub raw: String,
    owner: String,
    table: String,
}

impl TablePattern {
    pub fn parse(text: &str) -> Result<Self, String> {
        let Some((owner, table)) = text.split_once('.') else {
            return Err(format!("pattern {text} must be OWNER.TABLE"));
        };
        if owner.is_empty() || table.is_empty() {
            return Err(format!("pattern {text} must be OWNER.TABLE"));
        }
        Ok(Self {
            raw: text.to_string(),
            owner: owner.to_ascii_uppercase(),
            table: table.to_ascii_uppercase(),
        })
    }

    pub fn matches(&self, key: &TableKey) -> bool {
        glob_match(&self.owner, &key.owner.to_ascii_uppercase())
            && glob_match(&self.table, &key.table.to_ascii_uppercase())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Pulled,
    Missing,
    Group { members: Vec<TableKey> },
}

#[derive(Clone, Debug)]
pub struct Node {
    #[allow(dead_code)]
    pub key: TableKey,
    pub title: String,
    pub comment: Option<String>,
    pub primary_key: Vec<String>,
    pub kind: NodeKind,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeSource {
    ForeignKey { name: Option<String> },
    Relation { status: RelationStatus },
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub from: TableKey,
    pub to: TableKey,
    pub columns: Vec<String>,
    pub target_columns: Vec<String>,
    pub source: EdgeSource,
    pub kind: RelationKind,
    pub condition: Option<String>,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub nodes: BTreeMap<TableKey, Node>,
    pub edges: Vec<Edge>,
}

#[derive(Clone, Debug)]
pub struct Selection {
    pub roots: Vec<TablePattern>,
    pub depth: u8,
    pub groups: Vec<TablePattern>,
    pub confirmed_only: bool,
}

#[derive(Debug)]
pub enum SelectError {
    NoRootMatch(String),
    BadPattern(String),
}

pub fn build(docs: &[OntologyDoc]) -> Graph {
    let mut graph = Graph::default();
    for doc in docs {
        let key = TableKey::new(&doc.owner, &doc.table);
        graph.nodes.insert(
            key.clone(),
            Node {
                title: key.title(),
                comment: doc.table_comment.clone(),
                primary_key: doc.primary_key.clone(),
                kind: NodeKind::Pulled,
                key,
            },
        );
    }
    for doc in docs {
        let from = TableKey::new(&doc.owner, &doc.table);
        for foreign_key in &doc.foreign_keys {
            graph.edges.push(Edge {
                from: from.clone(),
                to: TableKey::new(&foreign_key.target_owner, &foreign_key.target_table),
                columns: foreign_key.columns.clone(),
                target_columns: foreign_key.target_columns.clone(),
                source: EdgeSource::ForeignKey {
                    name: foreign_key.name.clone(),
                },
                kind: RelationKind::Reference,
                condition: None,
                label: None,
            });
        }
        for relation in &doc.relations {
            let to = TableKey::new(&relation.target_owner, &relation.target_table);
            let same_foreign_key = graph.edges.iter().any(|edge| {
                edge.from == from
                    && edge.to == to
                    && same_columns(&edge.columns, &relation.columns)
                    && same_columns(&edge.target_columns, &relation.target_columns)
                    && matches!(edge.source, EdgeSource::ForeignKey { .. })
            });
            if same_foreign_key {
                continue;
            }
            graph.edges.push(Edge {
                from: from.clone(),
                to,
                columns: relation.columns.clone(),
                target_columns: relation.target_columns.clone(),
                source: EdgeSource::Relation {
                    status: relation.status,
                },
                kind: relation.kind,
                condition: relation.condition.clone(),
                label: relation.label.clone(),
            });
        }
    }
    let known: Vec<TableKey> = graph.nodes.keys().cloned().collect();
    for edge in &graph.edges {
        for key in [&edge.from, &edge.to] {
            if !known.iter().any(|item| item == key) && !graph.nodes.contains_key(key) {
                graph.nodes.insert(
                    key.clone(),
                    Node {
                        title: key.title(),
                        comment: None,
                        primary_key: Vec::new(),
                        kind: NodeKind::Missing,
                        key: key.clone(),
                    },
                );
            }
        }
    }
    graph.edges.sort_by(compare_edges);
    graph
}

pub fn select(graph: &Graph, selection: &Selection) -> Result<Graph, SelectError> {
    if selection.depth > 5 {
        return Err(SelectError::BadPattern(
            "depth must be from 0 to 5".to_string(),
        ));
    }
    let mut chosen: BTreeMap<TableKey, Node> = BTreeMap::new();
    if selection.roots.is_empty() {
        chosen.clone_from(&graph.nodes);
    } else {
        let mut seeds = Vec::new();
        for pattern in &selection.roots {
            let matched: Vec<TableKey> = graph
                .nodes
                .keys()
                .filter(|key| pattern.matches(key))
                .cloned()
                .collect();
            if matched.is_empty() {
                return Err(SelectError::NoRootMatch(pattern.raw.clone()));
            }
            seeds.extend(matched);
        }
        seeds.sort();
        seeds.dedup();
        let mut frontier = seeds.clone();
        for key in seeds {
            if let Some(node) = graph.nodes.get(&key) {
                chosen.insert(key, node.clone());
            }
        }
        for _ in 0..selection.depth {
            let mut next = Vec::new();
            for edge in &graph.edges {
                for (here, other) in [(&edge.from, &edge.to), (&edge.to, &edge.from)] {
                    if chosen.contains_key(here) && !chosen.contains_key(other) {
                        next.push(other.clone());
                    }
                }
            }
            next.sort();
            next.dedup();
            for key in next {
                if let Some(node) = graph.nodes.get(&key) {
                    chosen.insert(key.clone(), node.clone());
                    frontier.push(key);
                }
            }
        }
        let _ = frontier;
    }
    let mut edges: Vec<Edge> = graph
        .edges
        .iter()
        .filter(|edge| chosen.contains_key(&edge.from) && chosen.contains_key(&edge.to))
        .filter(|edge| {
            if !selection.confirmed_only {
                return true;
            }
            !matches!(
                edge.source,
                EdgeSource::Relation {
                    status: RelationStatus::Draft
                }
            )
        })
        .cloned()
        .collect();
    apply_groups(&mut chosen, &mut edges, &selection.groups);
    edges.sort_by(compare_edges);
    dedup_edges(&mut edges);
    Ok(Graph {
        nodes: chosen,
        edges,
    })
}

fn apply_groups(nodes: &mut BTreeMap<TableKey, Node>, edges: &mut [Edge], groups: &[TablePattern]) {
    for pattern in groups {
        let members: Vec<TableKey> = nodes
            .keys()
            .filter(|key| pattern.matches(key))
            .cloned()
            .collect();
        if members.len() < 2 {
            continue;
        }
        let Some((owner, table)) = pattern.raw.split_once('.') else {
            continue;
        };
        let key = TableKey::new(owner, table);
        let title = pattern.raw.clone();
        for member in &members {
            nodes.remove(member);
        }
        for edge in edges.iter_mut() {
            if members.iter().any(|member| member == &edge.from) {
                edge.from = key.clone();
            }
            if members.iter().any(|member| member == &edge.to) {
                edge.to = key.clone();
            }
        }
        nodes.insert(
            key.clone(),
            Node {
                title,
                comment: None,
                primary_key: Vec::new(),
                kind: NodeKind::Group { members },
                key,
            },
        );
    }
}

fn dedup_edges(edges: &mut Vec<Edge>) {
    edges.sort_by(compare_edges);
    let mut kept: Vec<Edge> = Vec::new();
    for edge in edges.drain(..) {
        let duplicate = kept
            .iter()
            .any(|existing| same_folded_edge(existing, &edge));
        if !duplicate {
            kept.push(edge);
        }
    }
    *edges = kept;
}

fn same_folded_edge(left: &Edge, right: &Edge) -> bool {
    left.from == right.from
        && left.to == right.to
        && same_columns(&left.columns, &right.columns)
        && same_columns(&left.target_columns, &right.target_columns)
        && status_tag(&left.source) == status_tag(&right.source)
        && left.kind == right.kind
        && left.condition == right.condition
        && left.label == right.label
}

fn status_tag(source: &EdgeSource) -> &'static str {
    match source {
        EdgeSource::ForeignKey { .. } => "foreign_key",
        EdgeSource::Relation {
            status: RelationStatus::Confirmed,
        } => "confirmed",
        EdgeSource::Relation {
            status: RelationStatus::Draft,
        } => "draft",
    }
}

fn same_columns(left: &[String], right: &[String]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

fn compare_edges(left: &Edge, right: &Edge) -> std::cmp::Ordering {
    (
        &left.from,
        &left.to,
        &left.columns,
        &left.target_columns,
        &left.source,
        left.kind,
        &left.condition,
        &left.label,
    )
        .cmp(&(
            &right.from,
            &right.to,
            &right.columns,
            &right.target_columns,
            &right.source,
            right.kind,
            &right.condition,
            &right.label,
        ))
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let mut state = vec![vec![false; text.len() + 1]; pattern.len() + 1];
    state[0][0] = true;
    for (index, pat) in pattern.iter().enumerate() {
        if *pat == '*' {
            state[index + 1][0] = state[index][0];
        }
    }
    for (i, pat) in pattern.iter().enumerate() {
        for (j, ch) in text.iter().enumerate() {
            state[i + 1][j + 1] = if *pat == '*' {
                state[i][j + 1] || state[i + 1][j]
            } else if *pat == '?' || pat.eq_ignore_ascii_case(ch) {
                state[i][j]
            } else {
                false
            };
        }
    }
    state[pattern.len()][text.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontology::schema::{OntologyColumn, OntologyDoc, Relation, VERSION};

    fn column(name: &str) -> OntologyColumn {
        OntologyColumn {
            name: name.to_string(),
            data_type: "TEXT".to_string(),
            data_length: None,
            data_precision: None,
            data_scale: None,
            nullable: false,
            is_virtual: false,
            db_comment: None,
            meaning: None,
            meaning_status: crate::ontology::schema::FieldStatus::Unknown,
            unit: None,
            unit_status: crate::ontology::schema::FieldStatus::Unknown,
        }
    }

    fn doc(
        table: &str,
        keys: Vec<crate::ontology::schema::ForeignKey>,
        relations: Vec<Relation>,
    ) -> OntologyDoc {
        let mut columns = vec![column("ID")];
        for key in &keys {
            for name in &key.columns {
                if !columns.iter().any(|column| column.name == *name) {
                    columns.push(column(name));
                }
            }
        }
        for relation in &relations {
            for name in &relation.columns {
                if !columns.iter().any(|column| column.name == *name) {
                    columns.push(column(name));
                }
            }
        }
        OntologyDoc {
            version: VERSION,
            source: "demo".to_string(),
            owner: "main".to_string(),
            table: table.to_string(),
            extracted_at: "2026-11-17T00:00:00Z".to_string(),
            object_type: "TABLE".to_string(),
            table_comment: None,
            primary_key: vec!["ID".to_string()],
            time_column: None,
            columns,
            foreign_keys: keys,
            relations,
        }
    }

    fn relation(to: &str, column_name: &str, status: RelationStatus) -> Relation {
        Relation {
            columns: vec![column_name.to_string()],
            target_owner: "main".to_string(),
            target_table: to.to_string(),
            target_columns: vec![column_name.to_string()],
            status,
            evidence: Some("same name".to_string()),
            overlap: None,
            kind: RelationKind::Reference,
            condition: None,
            label: None,
        }
    }

    #[test]
    fn graph_merges_relation_equal_to_foreign_key() {
        let foreign = crate::ontology::schema::ForeignKey {
            name: Some("FK_SITE".to_string()),
            columns: vec!["SITE_NO".to_string()],
            target_owner: "main".to_string(),
            target_table: "SITE".to_string(),
            target_columns: vec!["SITE_NO".to_string()],
        };
        let graph = build(&[
            doc(
                "SITE_ARM",
                vec![foreign],
                vec![relation("SITE", "SITE_NO", RelationStatus::Confirmed)],
            ),
            doc("SITE", Vec::new(), Vec::new()),
        ]);
        assert_eq!(graph.edges.len(), 1);
        assert!(matches!(
            graph.edges[0].source,
            EdgeSource::ForeignKey { .. }
        ));
    }

    #[test]
    fn graph_adds_missing_target_nodes() {
        let graph = build(&[doc(
            "FACT_A_1H",
            Vec::new(),
            vec![relation("GHOST", "ID", RelationStatus::Confirmed)],
        )]);
        let ghost = TableKey::new("main", "GHOST");
        assert!(matches!(
            graph.nodes.get(&ghost).map(|node| &node.kind),
            Some(NodeKind::Missing)
        ));
    }

    #[test]
    fn pattern_is_case_insensitive_glob_with_star_and_question() {
        let star = TablePattern::parse("main.FACT_A_*").unwrap();
        let question = TablePattern::parse("main.FACT_?_1H").unwrap();
        assert!(star.matches(&TableKey::new("MAIN", "fact_a_1d")));
        assert!(question.matches(&TableKey::new("main", "FACT_A_1H")));
        assert!(!question.matches(&TableKey::new("main", "FACT_A_1D")));
        assert!(TablePattern::parse("FACT").is_err());
    }

    #[test]
    fn select_root_depth_limits_neighbors() {
        let docs = vec![
            doc(
                "A",
                Vec::new(),
                vec![relation("B", "ID", RelationStatus::Confirmed)],
            ),
            doc(
                "B",
                Vec::new(),
                vec![relation("C", "ID", RelationStatus::Confirmed)],
            ),
            doc(
                "C",
                Vec::new(),
                vec![relation("D", "ID", RelationStatus::Confirmed)],
            ),
            doc("D", Vec::new(), Vec::new()),
        ];
        let graph = build(&docs);
        let root = vec![TablePattern::parse("main.A").unwrap()];
        let names = |depth: u8| {
            let selected = select(
                &graph,
                &Selection {
                    roots: root.clone(),
                    depth,
                    groups: Vec::new(),
                    confirmed_only: false,
                },
            )
            .unwrap();
            let mut tables: Vec<String> =
                selected.nodes.keys().map(|key| key.table.clone()).collect();
            tables.sort();
            tables
        };
        assert_eq!(names(0), ["A"]);
        assert_eq!(names(1), ["A", "B"]);
        assert_eq!(names(2), ["A", "B", "C"]);
    }

    #[test]
    fn select_group_collapses_family_and_dedupes_edges() {
        let graph = build(&[
            doc(
                "FACT_A_1H",
                Vec::new(),
                vec![relation("SITE", "SITE_NO", RelationStatus::Confirmed)],
            ),
            doc(
                "FACT_A_1D",
                Vec::new(),
                vec![relation("SITE", "SITE_NO", RelationStatus::Confirmed)],
            ),
            doc("SITE", Vec::new(), Vec::new()),
        ]);
        let selected = select(
            &graph,
            &Selection {
                roots: Vec::new(),
                depth: 2,
                groups: vec![TablePattern::parse("main.FACT_A_*").unwrap()],
                confirmed_only: false,
            },
        )
        .unwrap();
        assert!(selected.nodes.keys().any(|key| key.table == "FACT_A_*"));
        assert!(!selected.nodes.keys().any(|key| key.table == "FACT_A_1H"));
        assert_eq!(selected.edges.len(), 1);
    }

    #[test]
    fn select_confirmed_only_drops_draft_relations() {
        let graph = build(&[
            doc(
                "TOTAL_A_1H",
                Vec::new(),
                vec![relation("SITE", "SITE_NO", RelationStatus::Draft)],
            ),
            doc("SITE", Vec::new(), Vec::new()),
        ]);
        let selected = select(
            &graph,
            &Selection {
                roots: Vec::new(),
                depth: 2,
                groups: Vec::new(),
                confirmed_only: true,
            },
        )
        .unwrap();
        assert!(selected.edges.is_empty());
    }
}
