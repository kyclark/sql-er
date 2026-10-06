//! PlantUML entity diagram (Information Engineering notation).

use super::EmitOptions;
use crate::model::Schema;
use std::fmt::Write;

pub fn emit(schema: &Schema, opts: &EmitOptions) -> String {
    let mut out = String::new();
    writeln!(out, "@startuml").unwrap();
    if let Some(title) = &opts.title {
        writeln!(out, "title {title}").unwrap();
    }
    writeln!(out, "hide circle\nskinparam linetype ortho").unwrap();
    if opts.rankdir == "LR" || opts.rankdir == "RL" {
        writeln!(out, "left to right direction").unwrap();
    }
    for table in &schema.tables {
        writeln!(out).unwrap();
        let stereotype = if table.stub { " <<undefined>>" } else { "" };
        writeln!(
            out,
            "entity \"{}\" as {}{stereotype} {{",
            table.name,
            alias(&table.name)
        )
        .unwrap();
        let cols = opts.visible_columns(table);
        let (keys, rest): (Vec<_>, Vec<_>) =
            cols.into_iter().partition(|(_, c)| c.primary_key);
        for (_, col) in &keys {
            writeln!(out, "  * {}", attr(col, opts)).unwrap();
        }
        if !keys.is_empty() && !rest.is_empty() {
            writeln!(out, "  --").unwrap();
        }
        for (_, col) in &rest {
            let mandatory = if col.nullable { "" } else { "* " };
            writeln!(out, "  {mandatory}{}", attr(col, opts)).unwrap();
        }
        writeln!(out, "}}").unwrap();
    }
    if !schema.foreign_keys.is_empty() {
        writeln!(out).unwrap();
    }
    for fk in &schema.foreign_keys {
        let parent = if fk.is_optional(schema) { "|o" } else { "||" };
        let line = if fk.inferred { ".." } else { "--" };
        writeln!(
            out,
            "{} }}o{line}{parent} {}",
            alias(&fk.from_table),
            alias(&fk.to_table)
        )
        .unwrap();
    }
    writeln!(out, "@enduml").unwrap();
    out
}

fn attr(col: &crate::model::Column, opts: &EmitOptions) -> String {
    let mut s = col.name.clone();
    if opts.show_types && !col.data_type.is_empty() {
        s.push_str(&format!(" : {}", col.data_type));
    }
    if col.foreign_key {
        s.push_str(" <<FK>>");
    }
    s
}

fn alias(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
