use std::io::Write;
use std::process::{Command, Output, Stdio};

const SHOP: &str = "tests/data/shop.sql";

fn sql_er(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sql-er"))
        .args(args)
        .output()
        .expect("run sql-er")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn has_graphviz() -> bool {
    Command::new("dot").arg("-V").output().is_ok()
}

#[test]
fn json_model() {
    let out = sql_er(&["-d", "pg", "-f", "json", SHOP]);
    assert!(out.status.success(), "{}", stderr(&out));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let tables: Vec<&str> = json["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        tables,
        [
            "shop.customer",
            "shop.product",
            "shop.orders",
            "shop.order_line",
            "shop.audit",
            "shop.settings",
            "auth.users"
        ]
    );

    let fks: Vec<(String, String)> = json["foreign_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                format!(
                    "{}.{}",
                    f["from_table"].as_str().unwrap(),
                    f["from_columns"][0]
                ),
                format!("{}.{}", f["to_table"].as_str().unwrap(), f["to_columns"][0]),
            )
        })
        .collect();
    assert_eq!(fks.len(), 4);
    assert!(fks.contains(&(
        "shop.orders.\"customer_id\"".into(),
        "shop.customer.\"customer_id\"".into()
    )));
    assert!(stderr(&out).contains("skipped 1 statement"));
}

#[test]
fn dot_to_stdout_by_default() {
    let out = sql_er(&["-d", "pg", "-q", SHOP]);
    assert!(out.status.success());
    let dot = stdout(&out);
    assert!(dot.starts_with("digraph"));
    assert!(dot.contains(
        "\"shop.order_line\":\"c0\" -> \"shop.orders\":\"c0\" [arrowhead=teetee"
    ));
    assert!(
        dot.contains("STYLE=\"DASHED\""),
        "undefined table drawn as stub"
    );
    assert!(stderr(&out).is_empty(), "-q silences warnings");
}

#[test]
fn verbose_lists_each_warning() {
    let out = sql_er(&["-d", "pg", "-v", SHOP]);
    assert!(stderr(&out).contains("tests/data/shop.sql: sql parser error"));
}

#[test]
fn filters() {
    let out = sql_er(&[
        "-d",
        "pg",
        "-q",
        "-f",
        "json",
        "--skip-isolated",
        "-x",
        "audit",
        SHOP,
    ]);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<&str> = json["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"shop.settings"), "isolated");
    assert!(!names.contains(&"shop.audit"), "excluded");
    assert!(!names.contains(&"auth.users"), "stub no longer referenced");
    assert_eq!(names.len(), 4);
}

#[test]
fn mermaid_and_plantuml() {
    let out = sql_er(&["-d", "pg", "-q", "-f", "mermaid", SHOP]);
    let mmd = stdout(&out);
    assert!(mmd.starts_with("erDiagram"));
    assert!(mmd.contains("shop_customer[\"shop.customer\"] {"));
    assert!(mmd.contains("NUMERIC(10_2) price"));
    assert!(mmd.contains("shop_customer ||--o{ shop_orders : \"customer_id\""));

    let out = sql_er(&["-d", "pg", "-q", "-f", "puml", SHOP]);
    let puml = stdout(&out);
    assert!(puml.starts_with("@startuml"));
    assert!(puml.contains("shop_audit }o--|o auth_users"));
}

#[test]
fn reads_stdin() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sql-er"))
        .args(["-f", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"create table a (id int primary key); create table b (a_id int references a);")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["foreign_keys"].as_array().unwrap().len(), 1);
}

#[test]
fn no_tables_is_an_error() {
    let dir = std::env::temp_dir().join("sql-er-test-empty.sql");
    std::fs::write(&dir, "select 1;").unwrap();
    let out = sql_er(&[dir.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("no tables found"));
}

#[test]
fn renders_svg_from_output_extension() {
    if !has_graphviz() {
        eprintln!("skipping: GraphViz not installed");
        return;
    }
    let path = std::env::temp_dir().join("sql-er-test.svg");
    let out = sql_er(&["-d", "pg", "-q", "-o", path.to_str().unwrap(), SHOP]);
    assert!(out.status.success(), "{}", stderr(&out));
    let svg = std::fs::read_to_string(&path).unwrap();
    assert!(svg.contains("<svg"));
    assert!(svg.contains("shop.order_line"));
}

#[test]
fn missing_graphviz_is_explained() {
    let out = sql_er(&[
        "-d",
        "pg",
        "-q",
        "-f",
        "png",
        "-o",
        "/dev/null",
        "--graphviz",
        "/nonexistent/dot",
        SHOP,
    ]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("could not run GraphViz"));
}
