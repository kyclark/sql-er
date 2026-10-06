//! The minimal schema model needed to draw an E/R diagram.

use regex::Regex;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Schema {
    pub tables: Vec<Table>,
    pub foreign_keys: Vec<ForeignKey>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
    /// True for tables that are referenced by a foreign key but never defined.
    pub stub: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub primary_key: bool,
    pub foreign_key: bool,
    pub unique: bool,
    pub nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForeignKey {
    pub name: Option<String>,
    pub from_table: String,
    pub from_columns: Vec<String>,
    pub to_table: String,
    pub to_columns: Vec<String>,
    /// True when guessed from column names rather than declared in the DDL.
    pub inferred: bool,
}

impl Table {
    pub fn new(name: impl Into<String>) -> Self {
        Table {
            name: name.into(),
            ..Default::default()
        }
    }

    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn column_mut(&mut self, name: &str) -> Option<&mut Column> {
        self.columns
            .iter_mut()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn primary_key(&self) -> Vec<String> {
        self.columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| c.name.clone())
            .collect()
    }

    /// The last component of a possibly schema-qualified name.
    pub fn short_name(&self) -> &str {
        short_name(&self.name)
    }
}

impl ForeignKey {
    /// A relationship is optional when any of its referencing columns may be NULL.
    pub fn is_optional(&self, schema: &Schema) -> bool {
        schema.table(&self.from_table).is_some_and(|t| {
            self.from_columns
                .iter()
                .any(|c| t.column(c).is_none_or(|c| c.nullable))
        })
    }
}

pub fn short_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

impl Schema {
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.find_table(name).map(|i| &self.tables[i])
    }

    /// Find a table by name, case-insensitively. An unqualified name (`users`)
    /// matches a qualified one (`public.users`) and vice versa, as long as the
    /// match is unambiguous.
    pub fn find_table(&self, name: &str) -> Option<usize> {
        if let Some(i) = self
            .tables
            .iter()
            .position(|t| t.name.eq_ignore_ascii_case(name))
        {
            return Some(i);
        }
        let short = short_name(name);
        let mut hits = self
            .tables
            .iter()
            .enumerate()
            .filter(|(_, t)| t.short_name().eq_ignore_ascii_case(short));
        match (hits.next(), hits.next()) {
            (Some((i, _)), None) => Some(i),
            _ => None,
        }
    }

    /// Add a table, replacing any earlier definition with the same name
    /// (e.g. a migration that drops and recreates it).
    pub fn upsert_table(&mut self, table: Table) {
        match self
            .tables
            .iter()
            .position(|t| t.name.eq_ignore_ascii_case(&table.name))
        {
            Some(i) => {
                let old = std::mem::replace(&mut self.tables[i], table);
                self.foreign_keys
                    .retain(|fk| !fk.from_table.eq_ignore_ascii_case(&old.name));
            }
            None => self.tables.push(table),
        }
    }

    /// Point each foreign key at a defined table (adding stubs for undefined
    /// targets), fill in implicit target columns, flag FK columns, and drop
    /// duplicates.
    pub fn resolve_foreign_keys(&mut self) {
        let fks = std::mem::take(&mut self.foreign_keys);
        for mut fk in fks {
            let Some(from) = self.find_table(&fk.from_table) else {
                continue;
            };
            fk.from_table = self.tables[from].name.clone();

            let to = match self.find_table(&fk.to_table) {
                Some(i) => i,
                None => {
                    let mut stub = Table::new(fk.to_table.clone());
                    stub.stub = true;
                    self.tables.push(stub);
                    self.tables.len() - 1
                }
            };
            fk.to_table = self.tables[to].name.clone();
            if fk.to_columns.is_empty() {
                fk.to_columns = self.tables[to].primary_key();
            }

            for col in &fk.from_columns {
                if let Some(c) = self.tables[from].column_mut(col) {
                    c.foreign_key = true;
                }
            }
            if !self.foreign_keys.iter().any(|f| same_link(f, &fk)) {
                self.foreign_keys.push(fk);
            }
        }
    }

    /// Guess relationships that the DDL doesn't declare, from column names:
    /// a column matching another table's single-column primary key by name
    /// (other than a bare `id`), or `<table>_id` referencing `<table>` or
    /// `<table>s`.
    pub fn infer_foreign_keys(&mut self) {
        let pks: Vec<(usize, String)> = self
            .tables
            .iter()
            .enumerate()
            .filter_map(|(i, t)| match t.primary_key().as_slice() {
                [pk] => Some((i, pk.clone())),
                _ => None,
            })
            .collect();

        let mut inferred = vec![];
        for (ti, table) in self.tables.iter().enumerate() {
            for col in table.columns.iter().filter(|c| !c.foreign_key) {
                let name = col.name.to_ascii_lowercase();
                let target = pks
                    .iter()
                    .find(|(pi, pk)| {
                        *pi != ti && name != "id" && pk.eq_ignore_ascii_case(&name)
                    })
                    .or_else(|| {
                        let base = name.strip_suffix("_id")?;
                        pks.iter().find(|(pi, _)| {
                            let t = self.tables[*pi].short_name().to_ascii_lowercase();
                            *pi != ti
                                && (t == base
                                    || t == format!("{base}s")
                                    || t == format!("{base}es"))
                        })
                    });
                // When both sides use the column as their sole PK (1:1 tables),
                // there's no telling which is the parent, so don't guess.
                if let Some((pi, pk)) = target {
                    if col.primary_key && table.primary_key().len() == 1 {
                        continue;
                    }
                    inferred.push(ForeignKey {
                        name: None,
                        from_table: table.name.clone(),
                        from_columns: vec![col.name.clone()],
                        to_table: self.tables[*pi].name.clone(),
                        to_columns: vec![pk.clone()],
                        inferred: true,
                    });
                }
            }
        }
        for fk in inferred {
            if !self.foreign_keys.iter().any(|f| same_link(f, &fk)) {
                if let Some(ti) = self.find_table(&fk.from_table)
                    && let Some(c) = self.tables[ti].column_mut(&fk.from_columns[0])
                {
                    c.foreign_key = true;
                }
                self.foreign_keys.push(fk);
            }
        }
    }

    /// Drop the given schema prefixes (case-insensitively) from table names,
    /// so `public.users` becomes `users`. A table keeps its full name if the
    /// short one would clash with another table; those names are returned.
    pub fn strip_schemas(&mut self, schemas: &[String]) -> Vec<String> {
        let strip = |name: &str| -> Option<String> {
            let (schema, rest) = name.split_once('.')?;
            schemas
                .iter()
                .any(|s| s.eq_ignore_ascii_case(schema))
                .then(|| rest.to_string())
        };
        let new_names: Vec<String> = self
            .tables
            .iter()
            .map(|t| strip(&t.name).unwrap_or_else(|| t.name.clone()))
            .collect();

        let mut renames = HashMap::new();
        let mut collisions = vec![];
        for (i, table) in self.tables.iter().enumerate() {
            if new_names[i] == table.name {
                continue;
            }
            let clash = new_names
                .iter()
                .enumerate()
                .any(|(j, n)| j != i && n.eq_ignore_ascii_case(&new_names[i]));
            if clash {
                collisions.push(table.name.clone());
            } else {
                renames.insert(table.name.clone(), new_names[i].clone());
            }
        }

        for table in &mut self.tables {
            if let Some(new) = renames.get(&table.name) {
                table.name = new.clone();
            }
        }
        for fk in &mut self.foreign_keys {
            for name in [&mut fk.from_table, &mut fk.to_table] {
                if let Some(new) = renames.get(name.as_str()) {
                    *name = new.clone();
                }
            }
        }
        collisions
    }

    /// Keep only tables whose names match an `include` pattern (if any are
    /// given) and no `exclude` pattern. Foreign keys touching removed tables,
    /// and stubs nothing points at any more, are dropped too.
    pub fn filter(
        &mut self,
        include: &[Regex],
        exclude: &[Regex],
        skip_isolated: bool,
    ) {
        self.tables.retain(|t| {
            (t.stub
                || include.is_empty()
                || include.iter().any(|r| r.is_match(&t.name)))
                && !exclude.iter().any(|r| r.is_match(&t.name))
        });
        let names: HashSet<String> =
            self.tables.iter().map(|t| t.name.clone()).collect();
        self.foreign_keys.retain(|fk| {
            names.contains(&fk.from_table) && names.contains(&fk.to_table)
        });

        let mut linked: HashMap<&str, usize> = HashMap::new();
        for fk in &self.foreign_keys {
            *linked.entry(fk.from_table.as_str()).or_default() += 1;
            *linked.entry(fk.to_table.as_str()).or_default() += 1;
        }
        let linked: HashSet<String> = linked.keys().map(|s| s.to_string()).collect();
        self.tables
            .retain(|t| linked.contains(&t.name) || !(t.stub || skip_isolated));
    }
}

fn same_link(a: &ForeignKey, b: &ForeignKey) -> bool {
    let eq = |x: &[String], y: &[String]| {
        x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.eq_ignore_ascii_case(q))
    };
    a.from_table == b.from_table
        && a.to_table == b.to_table
        && eq(&a.from_columns, &b.from_columns)
        && eq(&a.to_columns, &b.to_columns)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, pk: bool) -> Column {
        Column {
            name: name.into(),
            data_type: "int".into(),
            primary_key: pk,
            foreign_key: false,
            unique: false,
            nullable: !pk,
        }
    }

    fn table(name: &str, cols: Vec<Column>) -> Table {
        Table {
            name: name.into(),
            columns: cols,
            stub: false,
        }
    }

    #[test]
    fn find_table_matches_qualified_and_unqualified() {
        let mut s = Schema::default();
        s.tables.push(table("public.Users", vec![]));
        assert_eq!(s.find_table("users"), Some(0));
        assert_eq!(s.find_table("PUBLIC.USERS"), Some(0));
        s.tables.push(table("other.users", vec![]));
        assert_eq!(s.find_table("users"), None, "ambiguous short name");
    }

    #[test]
    fn resolve_adds_stub_and_implicit_pk() {
        let mut s = Schema::default();
        s.tables.push(table("users", vec![col("id", true)]));
        s.tables.push(table(
            "orders",
            vec![col("id", true), col("user_id", false)],
        ));
        s.foreign_keys.push(ForeignKey {
            name: None,
            from_table: "orders".into(),
            from_columns: vec!["user_id".into()],
            to_table: "USERS".into(),
            to_columns: vec![],
            inferred: false,
        });
        s.foreign_keys.push(ForeignKey {
            name: None,
            from_table: "orders".into(),
            from_columns: vec!["id".into()],
            to_table: "missing".into(),
            to_columns: vec!["x".into()],
            inferred: false,
        });
        s.resolve_foreign_keys();
        assert_eq!(s.foreign_keys[0].to_table, "users");
        assert_eq!(s.foreign_keys[0].to_columns, vec!["id"]);
        assert!(s.tables[1].column("user_id").unwrap().foreign_key);
        assert!(s.tables[2].stub);
    }

    #[test]
    fn infer_from_names() {
        let mut s = Schema::default();
        s.tables.push(table("users", vec![col("id", true)]));
        s.tables
            .push(table("category", vec![col("category_id", true)]));
        s.tables.push(table(
            "orders",
            vec![
                col("id", true),
                col("user_id", false),
                col("category_id", false),
            ],
        ));
        s.infer_foreign_keys();
        let links: Vec<_> = s
            .foreign_keys
            .iter()
            .map(|f| (f.from_columns[0].as_str(), f.to_table.as_str()))
            .collect();
        assert!(links.contains(&("user_id", "users")));
        assert!(s.tables[2].column("user_id").unwrap().foreign_key);
        assert!(links.contains(&("category_id", "category")));
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn strip_schemas_renames_tables_and_fks() {
        let mut s = Schema::default();
        s.tables.push(table("public.users", vec![col("id", true)]));
        s.tables
            .push(table("Public.orders", vec![col("user_id", false)]));
        s.tables.push(table("auth.users", vec![col("id", true)]));
        s.tables
            .push(table("auth.sessions", vec![col("user_id", false)]));
        s.tables.push(table("nodot", vec![]));
        s.foreign_keys.push(ForeignKey {
            name: None,
            from_table: "Public.orders".into(),
            from_columns: vec!["user_id".into()],
            to_table: "public.users".into(),
            to_columns: vec!["id".into()],
            inferred: false,
        });

        let clashes = s.strip_schemas(&["public".into()]);
        assert!(clashes.is_empty());
        let names: Vec<_> = s.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            ["users", "orders", "auth.users", "auth.sessions", "nodot"]
        );
        assert_eq!(s.foreign_keys[0].from_table, "orders");
        assert_eq!(s.foreign_keys[0].to_table, "users");

        // Stripping auth too would leave two tables called "users".
        let clashes = s.strip_schemas(&["auth".into()]);
        assert_eq!(clashes, ["auth.users"]);
        assert!(s.tables.iter().any(|t| t.name == "sessions"));
    }

    #[test]
    fn filter_drops_edges_and_isolated() {
        let mut s = Schema::default();
        s.tables.push(table("a", vec![col("id", true)]));
        s.tables.push(table("b", vec![col("a_id", false)]));
        s.tables.push(table("c", vec![]));
        s.foreign_keys.push(ForeignKey {
            name: None,
            from_table: "b".into(),
            from_columns: vec!["a_id".into()],
            to_table: "a".into(),
            to_columns: vec!["id".into()],
            inferred: false,
        });
        let mut t = s.clone();
        t.filter(&[], &[], true);
        assert_eq!(t.tables.len(), 2);
        s.filter(&[], &[Regex::new("^a$").unwrap()], false);
        assert_eq!(s.tables.len(), 2);
        assert!(s.foreign_keys.is_empty());
    }
}
