//! Mermaid `erDiagram`, which GitHub and GitLab render inline in Markdown.

use super::EmitOptions;
use crate::model::Schema;
use std::fmt::Write;

pub fn emit(schema: &Schema, opts: &EmitOptions) -> String {
    let mut out = String::new();
    if let Some(title) = &opts.title {
        writeln!(out, "---\ntitle: {title}\n---").unwrap();
    }
    writeln!(out, "erDiagram").unwrap();
    for table in &schema.tables {
        let cols = opts.visible_columns(table);
        if cols.is_empty() {
            writeln!(out, "    {}", entity(&table.name)).unwrap();
            continue;
        }
        writeln!(out, "    {} {{", entity(&table.name)).unwrap();
        for (_, col) in cols {
            let mut keys = vec![];
            if col.primary_key {
                keys.push("PK");
            }
            if col.foreign_key {
                keys.push("FK");
            }
            if col.unique && !col.primary_key {
                keys.push("UK");
            }
            // Mermaid requires a type, so use a placeholder when types are hidden.
            let ty = if opts.show_types {
                type_name(&col.data_type)
            } else {
                "_".into()
            };
            write!(out, "        {ty} {}", ident(&col.name)).unwrap();
            if !keys.is_empty() {
                write!(out, " {}", keys.join(", ")).unwrap();
            }
            writeln!(out).unwrap();
        }
        writeln!(out, "    }}").unwrap();
    }
    for fk in &schema.foreign_keys {
        let parent = if fk.is_optional(schema) { "|o" } else { "||" };
        let line = if fk.inferred { ".." } else { "--" };
        writeln!(
            out,
            "    {} {parent}{line}o{{ {} : \"{}\"",
            ident(&fk.to_table),
            ident(&fk.from_table),
            fk.from_columns.join(", ").replace('"', "'")
        )
        .unwrap();
    }
    out
}

/// The entity declaration, with an `["alias"]` showing the real name when it
/// had to be sanitized.
fn entity(name: &str) -> String {
    let id = ident(name);
    if id == name {
        id
    } else {
        format!("{id}[\"{}\"]", name.replace('"', "'"))
    }
}

/// Mermaid entity and attribute names must be word characters.
fn ident(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Types may contain parentheses but not spaces or commas.
fn type_name(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_-()[]".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if t.is_empty() { "_".into() } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names_and_types() {
        assert_eq!(ident("public.my table"), "public_my_table");
        assert_eq!(type_name("NUMERIC(10,2)"), "NUMERIC(10_2)");
        assert_eq!(type_name("double precision"), "double_precision");
    }
}
