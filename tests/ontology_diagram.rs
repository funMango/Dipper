mod common;

use std::path::{Path, PathBuf};

fn copy_fixture(dir: &Path) {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/diagram");
    for folder in ["extracted", "overlay"] {
        let from = source.join(folder);
        copy_tree(&from, &dir.join("ontology").join(folder));
    }
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in walkdir_files(from) {
        let relative = entry.strip_prefix(from).unwrap();
        let dest = to.join(relative);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::copy(&entry, &dest).unwrap();
    }
}

fn walkdir_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn hashes(dir: &Path) -> Vec<(String, String)> {
    walkdir_files(dir)
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(dir)
                .unwrap()
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            (relative, common::sha256_file(&path))
        })
        .collect()
}

fn run(dir: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    common::dipper().current_dir(dir).args(args).assert()
}

#[test]
fn diagram_writes_svg_under_diagrams_source_name() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(dir.path());
    run(
        dir.path(),
        &["ontology", "diagram", "demo", "--root", "main.FACT_A_*"],
    )
    .code(0);
    assert!(dir
        .path()
        .join("ontology/diagrams/demo/main.fact_a__.svg")
        .is_file());
}

#[test]
fn diagram_svg_keeps_text_elements_and_uses_no_forbidden_tags() {
    let svg = diagram_svg(&["--root", "main.FACT_A_*"]);
    let text_nodes = svg.matches("<text").count();
    let nodes = svg.matches("<g id=\"node-").count();
    let edges = svg.matches("<g id=\"edge-").count();
    assert!(text_nodes >= nodes + edges, "{text_nodes} {nodes} {edges}");
    for forbidden in [
        "<foreignObject",
        "<style",
        "<script",
        "<image",
        "<use",
        "<marker",
        "<defs",
        "href=",
        "xlink:href",
        "@font-face",
        "<filter",
    ] {
        assert!(!svg.contains(forbidden), "{forbidden}");
    }
    assert_tags_balanced(&svg);
}

#[test]
fn diagram_draft_edges_are_dashed_and_confirmed_edges_are_solid() {
    let svg = diagram_svg(&["--root", "main.FACT_A_*"]);
    let drafts = svg.matches("data-status=\"draft\"").count();
    assert!(drafts >= 1, "{svg}");
    let dashed = draft_paths(&svg);
    assert_eq!(dashed, drafts);
    assert_eq!(solid_dash_count(&svg), 0);
}

#[test]
fn diagram_labels_show_key_columns_condition_and_rollup() {
    let svg = diagram_svg(&["--root", "main.FACT_A_*"]);
    assert!(svg.contains("SITE_NO,ARM_NO"), "{svg}");
    assert!(
        svg.contains("KIND_CD → ITEM_CD (GROUP_CD=&apos;KIND&apos;)"),
        "{svg}"
    );
    assert!(svg.contains("합계 = 전 방향 SUM"), "{svg}");
    assert!(svg.contains("FK SITE_NO"), "{svg}");
    assert!(
        svg.contains(">main.SITE_ARM<") || svg.contains(">main.SITE_ARM<"),
        "{svg}"
    );
}

#[test]
fn diagram_root_pattern_selects_roots_and_neighbors() {
    let shallow = diagram_svg(&["--root", "main.FACT_A_*", "--depth", "0"]);
    assert!(shallow.contains("id=\"node-main.FACT_A_1H\""));
    assert!(!shallow.contains("id=\"node-main.SITE_ARM\""));
    let near = diagram_svg(&["--root", "main.FACT_A_*", "--depth", "1"]);
    assert!(near.contains("id=\"node-main.SITE_ARM\""));
    assert!(!near.contains("id=\"node-main.CODE_GROUP\""));
}

#[test]
fn diagram_root_without_match_exits_1_with_object_not_found() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(dir.path());
    let output = run(
        dir.path(),
        &[
            "ontology",
            "diagram",
            "demo",
            "--root",
            "main.NOPE*",
            "--format",
            "json",
        ],
    )
    .code(1)
    .get_output()
    .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(doc["error"]["code"], "object_not_found");
    assert_eq!(doc["error"]["retryable"], false);
    assert!(doc["error"]["message"]
        .as_str()
        .unwrap()
        .contains("main.NOPE*"));
}

#[test]
fn diagram_rejects_unsafe_name_or_pattern_with_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(dir.path());
    run(
        dir.path(),
        &["ontology", "diagram", "demo", "--name", "../x"],
    )
    .code(2);
    run(
        dir.path(),
        &["ontology", "diagram", "demo", "--root", "FACT"],
    )
    .code(2);
    run(dir.path(), &["ontology", "diagram", "demo", "--depth", "6"]).code(2);
}

#[test]
fn diagram_confirmed_only_hides_draft() {
    let svg = diagram_svg(&["--root", "main.FACT_A_*", "--confirmed-only"]);
    assert!(!svg.contains("data-status=\"draft\""));
    assert!(svg.contains("data-status=\"confirmed\""));
}

#[test]
fn diagram_group_draws_one_box_for_family() {
    let svg = diagram_svg(&[
        "--root",
        "main.FACT_A_*",
        "--group",
        "main.FACT_A_*",
        "--depth",
        "0",
    ]);
    assert!(svg.contains("id=\"node-main.FACT_A_*\""));
    assert!(svg.contains(">2 tables<"));
    assert!(!svg.contains("id=\"node-main.FACT_A_1H\""));
    assert!(!svg.contains("id=\"node-main.FACT_A_1D\""));
}

#[test]
fn diagram_is_byte_identical_across_runs_and_file_creation_order() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(dir.path());
    let args = [
        "ontology",
        "diagram",
        "demo",
        "--root",
        "main.FACT_A_*",
        "--mmd",
    ];
    run(dir.path(), &args).code(0);
    let first = hashes(&dir.path().join("ontology/diagrams"));
    run(dir.path(), &args).code(0);
    let second = hashes(&dir.path().join("ontology/diagrams"));
    assert_eq!(first, second);
}

#[test]
fn diagram_matches_golden_svg_and_mmd() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(dir.path());
    run(
        dir.path(),
        &[
            "ontology",
            "diagram",
            "demo",
            "--root",
            "main.FACT_A_*",
            "--mmd",
        ],
    )
    .code(0);
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/diagram");
    assert_eq!(
        std::fs::read(dir.path().join("ontology/diagrams/demo/main.fact_a__.svg")).unwrap(),
        std::fs::read(golden.join("demo_fact_a.svg")).unwrap()
    );
    assert_eq!(
        std::fs::read(dir.path().join("ontology/diagrams/demo/main.fact_a__.mmd")).unwrap(),
        std::fs::read(golden.join("demo_fact_a.mmd")).unwrap()
    );
}

#[test]
fn diagram_runs_without_config_file_or_database() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(dir.path());
    assert!(!dir.path().join("dipper.toml").exists());
    run(dir.path(), &["ontology", "diagram", "demo"]).code(0);
}

#[test]
fn diagram_json_output_matches_schema() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(dir.path());
    let output = run(
        dir.path(),
        &[
            "ontology",
            "diagram",
            "demo",
            "--root",
            "main.FACT_A_*",
            "--format",
            "json",
        ],
    )
    .code(0)
    .get_output()
    .clone();
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["command"], "ontology.diagram");
    assert_eq!(doc["source"], "demo");
    assert_eq!(doc["roots"][0], "main.FACT_A_*");
    assert_eq!(doc["depth"], 2);
    assert_eq!(doc["name"], "main.fact_a__");
    assert_eq!(doc["files"][0], "ontology/diagrams/demo/main.fact_a__.svg");
    assert!(doc["tables"].as_u64().unwrap() >= 1);
    assert!(doc["edges"]["confirmed"].is_number());
    assert!(doc["edges"]["draft"].is_number());
    assert!(doc["edges"]["foreign_key"].is_number());
    assert!(doc["warnings"].is_array());
    assert!(doc["error"].is_null());
}

#[test]
fn diagram_leaves_extracted_and_overlay_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(dir.path());
    let before = hashes(&dir.path().join("ontology/extracted"));
    let overlay = hashes(&dir.path().join("ontology/overlay"));
    run(dir.path(), &["ontology", "diagram", "demo"]).code(0);
    assert_eq!(hashes(&dir.path().join("ontology/extracted")), before);
    assert_eq!(hashes(&dir.path().join("ontology/overlay")), overlay);
}

#[test]
fn diagram_escapes_xml_in_names_and_labels() {
    let svg = diagram_svg(&[]);
    assert!(svg.contains("A&lt;B&gt;&amp;&quot;C&quot;&apos;"), "{svg}");
    assert!(!svg.contains("A<B>"));
}

fn diagram_svg(args: &[&str]) -> String {
    let dir = tempfile::tempdir().unwrap();
    copy_fixture(dir.path());
    let mut full = vec!["ontology", "diagram", "demo"];
    full.extend_from_slice(args);
    run(dir.path(), &full).code(0);
    let svg = walkdir_files(&dir.path().join("ontology/diagrams"))
        .into_iter()
        .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("svg"))
        .unwrap();
    std::fs::read_to_string(svg).unwrap()
}

fn draft_paths(svg: &str) -> usize {
    svg.split("data-status=\"draft\"")
        .skip(1)
        .filter(|part| {
            part.lines().nth(1).is_some_and(|line| {
                line.contains("<path") && line.contains("stroke-dasharray=\"6 4\"")
            })
        })
        .count()
}

fn solid_dash_count(svg: &str) -> usize {
    ["data-status=\"confirmed\"", "data-status=\"foreign_key\""]
        .into_iter()
        .map(|marker| {
            svg.split(marker)
                .skip(1)
                .filter(|part| {
                    part.lines().nth(1).is_some_and(|line| {
                        line.contains("<path") && line.contains("stroke-dasharray")
                    })
                })
                .count()
        })
        .sum()
}

fn assert_tags_balanced(svg: &str) {
    for tag in ["svg", "g", "text", "tspan"] {
        let open = svg.matches(&format!("<{tag}")).count();
        let close = svg.matches(&format!("</{tag}>")).count();
        assert_eq!(open, close, "{tag}");
    }
}
