//! GraphViz DOT with crow's-foot notation.

use super::EmitOptions;
use crate::model::Schema;
use std::fmt::Write;

const HEADER_BG: &str = "#dbe7f3";
const STUB_BG: &str = "#eeeeee";
const MUTED: &str = "#666666";

pub fn emit(schema: &Schema, opts: &EmitOptions) -> String {
    let mut out = String::new();
    let title = opts.title.as_deref().unwrap_or("schema");
    writeln!(out, "digraph {} {{", quote(title)).unwrap();
    writeln!(
        out,
        "  graph [rankdir={}, fontname=\"Helvetica\", nodesep=0.4, ranksep=1.0{}];",
        opts.rankdir,
        opts.title
            .as_deref()
            .map(|t| format!(", label={}, labelloc=t, fontsize=16", quote(t)))
            .unwrap_or_default()
    )
    .unwrap();
    writeln!(
        out,
        "  node [shape=plain, fontname=\"Helvetica\", fontsize=10];"
    )
    .unwrap();
    writeln!(
        out,
        "  edge [dir=both, arrowtail=crowodot, color=\"#555555\", fontname=\"Helvetica\", fontsize=9];"
    )
    .unwrap();

    for table in &schema.tables {
        let bg = if table.stub { STUB_BG } else { HEADER_BG };
        let style = if table.stub { " STYLE=\"DASHED\"" } else { "" };
        writeln!(out).unwrap();
        writeln!(out, "  {} [label=<", quote(&table.name)).unwrap();
        writeln!(
            out,
            "    <TABLE BORDER=\"1\" CELLBORDER=\"0\" CELLSPACING=\"0\" CELLPADDING=\"4\"{style}>"
        )
        .unwrap();
        writeln!(
            out,
            "      <TR><TD BGCOLOR=\"{bg}\"><B>{}</B></TD></TR>",
            html(&table.name)
        )
        .unwrap();
        for (i, col) in opts.visible_columns(table) {
            let mut marks = vec![];
            if col.primary_key {
                marks.push("PK");
            }
            if col.foreign_key {
                marks.push("FK");
            }
            if col.unique && !col.primary_key {
                marks.push("U");
            }
            let mut name = html(&col.name);
            if col.primary_key {
                name = format!("<B>{name}</B>");
            }
            if !col.nullable && !col.primary_key {
                name = format!("{name}*");
            }
            let marks = if marks.is_empty() {
                String::new()
            } else {
                format!(
                    "<FONT POINT-SIZE=\"8\" COLOR=\"{MUTED}\">{}</FONT> ",
                    marks.join(",")
                )
            };
            let ty = if opts.show_types && !col.data_type.is_empty() {
                format!(" <FONT COLOR=\"{MUTED}\">{}</FONT>", html(&col.data_type))
            } else {
                String::new()
            };
            writeln!(
                out,
                "      <TR><TD PORT=\"c{i}\" ALIGN=\"LEFT\">{marks}{name}{ty}</TD></TR>"
            )
            .unwrap();
        }
        writeln!(out, "    </TABLE>").unwrap();
        writeln!(out, "  >];").unwrap();
    }

    if !schema.foreign_keys.is_empty() {
        writeln!(out).unwrap();
    }
    for fk in &schema.foreign_keys {
        let from = endpoint(schema, &fk.from_table, fk.from_columns.first(), opts);
        let to = endpoint(schema, &fk.to_table, fk.to_columns.first(), opts);
        let mut attrs = vec![format!(
            "arrowhead={}",
            if fk.is_optional(schema) {
                "teeodot"
            } else {
                "teetee"
            }
        )];
        if fk.inferred {
            attrs.push("style=dashed".into());
        }
        let tip = format!(
            "{}({}) -> {}({})",
            fk.from_table,
            fk.from_columns.join(", "),
            fk.to_table,
            fk.to_columns.join(", ")
        );
        attrs.push(format!("tooltip={}", quote(&tip)));
        writeln!(out, "  {from} -> {to} [{}];", attrs.join(", ")).unwrap();
    }
    writeln!(out, "}}").unwrap();
    out
}

/// `"table":"cN"` when the column is drawn, otherwise just the table node.
fn endpoint(
    schema: &Schema,
    table: &str,
    column: Option<&String>,
    opts: &EmitOptions,
) -> String {
    let port = schema.table(table).and_then(|t| {
        let i = t.column_index(column?)?;
        opts.visible_columns(t)
            .iter()
            .any(|(j, _)| *j == i)
            .then(|| format!(":\"c{i}\""))
    });
    format!("{}{}", quote(table), port.unwrap_or_default())
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, ForeignKey, Table};

    fn sample() -> Schema {
        let col = |n: &str, pk, fk| Column {
            name: n.into(),
            data_type: "varchar(10)".into(),
            primary_key: pk,
            foreign_key: fk,
            unique: false,
            nullable: !pk,
        };
        Schema {
            tables: vec![
                Table {
                    name: "a<b>".into(),
                    columns: vec![col("id", true, false)],
                    ..Default::default()
                },
                Table {
                    name: "child".into(),
                    columns: vec![
                        col("id", true, false),
                        col("note", false, false),
                        col("a_id", false, true),
                    ],
                    ..Default::default()
                },
            ],
            foreign_keys: vec![ForeignKey {
                name: None,
                from_table: "child".into(),
                from_columns: vec!["a_id".into()],
                to_table: "a<b>".into(),
                to_columns: vec!["id".into()],
                inferred: false,
            }],
        }
    }

    #[test]
    fn escapes_and_ports() {
        let dot = emit(&sample(), &EmitOptions::default());
        assert!(dot.contains("<B>a&lt;b&gt;</B>"));
        assert!(dot.contains("\"child\":\"c2\" -> \"a<b>\":\"c0\" [arrowhead=teeodot"));
    }

    #[test]
    fn keys_only_keeps_ports_valid() {
        let opts = EmitOptions {
            keys_only: true,
            show_types: false,
            ..Default::default()
        };
        let dot = emit(&sample(), &opts);
        assert!(!dot.contains("note"));
        assert!(!dot.contains("varchar"));
        assert!(dot.contains("\"child\":\"c2\""));
    }
}
