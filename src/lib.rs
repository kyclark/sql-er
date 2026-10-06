//! Draw entity-relationship diagrams from SQL DDL.

pub mod emit;
pub mod model;
pub mod parse;
pub mod render;

pub use model::Schema;
pub use parse::Warning;

use sqlparser::dialect::Dialect;

/// Parse one or more DDL sources into a single schema with resolved foreign keys.
pub fn schema_from_sql<'a>(
    dialect: &dyn Dialect,
    sources: impl IntoIterator<Item = &'a str>,
) -> (Schema, Vec<Warning>) {
    let mut statements = vec![];
    let mut warnings = vec![];
    for sql in sources {
        let (s, w) = parse::parse_statements(dialect, sql);
        statements.extend(s);
        warnings.extend(w);
    }
    let mut schema = Schema::default();
    warnings.extend(parse::add_statements(&mut schema, &statements));
    schema.resolve_foreign_keys();
    (schema, warnings)
}
