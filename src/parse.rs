//! Turn DDL text into a [`Schema`], skipping statements that don't parse.

use crate::model::{Column, ForeignKey, NamedConstraint, Schema, Table};
use regex::Regex;
use sqlparser::ast::{
    AlterColumnOperation, AlterTable, AlterTableOperation, ColumnDef, ColumnOption,
    CreateTable, DataType, Expr, ForeignKeyConstraint, IndexColumn, ObjectName,
    ObjectNamePart, ObjectType, RenameTableNameKind, Statement, TableConstraint,
};
use sqlparser::dialect::{Dialect, SQLiteDialect};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, TokenWithSpan, Tokenizer, Whitespace};
use std::sync::LazyLock;

/// Statements sqlparser can't parse that add nothing to a diagram, so they
/// are skipped without a warning: pg_dump's `ALTER TABLE ... ALTER COLUMN
/// ... ADD GENERATED ... AS IDENTITY (SEQUENCE NAME ...)` (sqlparser stops
/// at `SEQUENCE NAME`; the column's `NOT NULL` is already in its `CREATE
/// TABLE`). Matched against the statement's tokens joined by spaces.
static HARMLESS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(
            r"(?is)^ALTER TABLE (?:ONLY )?.+? ALTER (?:COLUMN )?.+? ADD GENERATED\b",
        )
        .expect("valid regex"),
    ]
});

/// A statement that was skipped, with the parser's explanation.
#[derive(Debug, Clone)]
pub struct Warning {
    pub message: String,
}

/// Rewrite client-side batch syntax that sqlparser doesn't understand into
/// plain `;` terminators, keeping line numbers intact:
/// `GO` (SQL Server/Sybase), a lone `/` (Oracle SQL*Plus), and MySQL's
/// `DELIMITER` command. psql meta-commands (a line starting with `\`, such
/// as the `\restrict` pair pg_dump writes since 17.6) are dropped.
pub fn preprocess(sql: &str) -> String {
    let mut delimiter = String::from(";");
    let mut out = String::with_capacity(sql.len());
    for line in sql.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let eol = &line[body.len()..];
        let trimmed = body.trim();
        let mut words = trimmed.split_whitespace();
        if trimmed.starts_with('\\') {
            out.push_str(eol);
        } else if words
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case("delimiter"))
        {
            if let Some(d) = words.next() {
                delimiter = d.to_string();
            }
            out.push_str(eol);
        } else if trimmed.eq_ignore_ascii_case("go") || trimmed == "/" {
            out.push(';');
            out.push_str(eol);
        } else if delimiter != ";" && trimmed.ends_with(delimiter.as_str()) {
            let cut = body.rfind(delimiter.as_str()).unwrap_or(body.len());
            out.push_str(&body[..cut]);
            out.push(';');
            out.push_str(eol);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Parse as many statements as possible. When one fails, record a warning
/// and resume after the next `;`.
pub fn parse_statements(
    dialect: &dyn Dialect,
    sql: &str,
) -> (Vec<Statement>, Vec<Warning>) {
    let sql = preprocess(sql);
    let mut statements = vec![];
    let mut warnings = vec![];

    let tokens: Vec<TokenWithSpan> =
        match Tokenizer::new(dialect, &sql).tokenize_with_location() {
            Ok(t) => normalize_tokens(dialect, t),
            Err(e) => {
                warnings.push(Warning {
                    message: format!("could not tokenize input: {e}"),
                });
                return (statements, warnings);
            }
        };

    let is_ws: Vec<bool> = tokens
        .iter()
        .map(|t| matches!(t.token, Token::Whitespace(_)))
        .collect();
    let mut parser = Parser::new(dialect).with_tokens_with_locations(tokens);
    loop {
        while parser.consume_token(&Token::SemiColon) {}
        if parser.peek_token_ref().token == Token::EOF {
            break;
        }
        // The statement's first real token, so that rewinding below stops
        // exactly here rather than backing over whitespace into the previous
        // statement.
        let stmt_start = (parser.index()..is_ws.len())
            .find(|&i| !is_ws[i])
            .unwrap_or(is_ws.len());
        match parser.parse_statement() {
            Ok(stmt) => {
                statements.push(stmt);
                let next = parser.peek_token();
                if !matches!(next.token, Token::SemiColon | Token::EOF) {
                    warnings.push(Warning {
                        message: format!(
                            "ignored unexpected {} after statement at {}",
                            next.token, next.span.start
                        ),
                    });
                    skip_past_semicolon(&mut parser);
                }
            }
            Err(e) => {
                // The parser may have stopped anywhere, even past a `;`, so
                // rewind to where the statement began and skip just that one.
                // (Rebuilding the parser instead would copy the remaining
                // tokens on every error, which is quadratic.)
                while parser.index() > stmt_start {
                    parser.prev_token();
                }
                let skipped = skip_past_semicolon(&mut parser);
                if !HARMLESS.iter().any(|r| r.is_match(&skipped)) {
                    warnings.push(Warning {
                        message: e.to_string(),
                    });
                }
            }
        }
    }
    (statements, warnings)
}

/// Patch up token-level syntax that sqlparser rejects but that's harmless
/// to drop or reinterpret for diagramming:
///
/// * Oracle constraint states (`NOT NULL ENABLE`, `... REFERENCES t (id) ENABLE`)
///   right before a `,` or `)`.
/// * SQLite column names written as string literals (`'name' varchar(20)`).
fn normalize_tokens(
    dialect: &dyn Dialect,
    mut tokens: Vec<TokenWithSpan>,
) -> Vec<TokenWithSpan> {
    const STATES: [&str; 6] = [
        "ENABLE",
        "DISABLE",
        "VALIDATE",
        "NOVALIDATE",
        "RELY",
        "NORELY",
    ];
    let is_state = |t: &Token| {
        matches!(t, Token::Word(w) if w.quote_style.is_none()
            && STATES.iter().any(|s| w.value.eq_ignore_ascii_case(s)))
    };
    let is_ws = |t: &Token| matches!(t, Token::Whitespace(_));
    let sqlite = dialect.is::<SQLiteDialect>();

    for i in 0..tokens.len() {
        if is_state(&tokens[i].token) {
            let next = tokens[i + 1..]
                .iter()
                .find(|t| !is_ws(&t.token) && !is_state(&t.token));
            if next.is_some_and(|t| matches!(t.token, Token::Comma | Token::RParen)) {
                tokens[i].token = Token::Whitespace(Whitespace::Space);
            }
        } else if sqlite && let Token::SingleQuotedString(name) = &tokens[i].token {
            let prev = tokens[..i].iter().rev().find(|t| !is_ws(&t.token));
            let next = tokens[i + 1..].iter().find(|t| !is_ws(&t.token));
            if prev.is_some_and(|t| matches!(t.token, Token::LParen | Token::Comma))
                && next.is_some_and(|t| matches!(t.token, Token::Word(_)))
            {
                tokens[i].token = Token::make_word(name, Some('"'));
            }
        }
    }
    tokens
}

/// Skip to just past the next `;`, returning the skipped tokens joined by
/// spaces.
fn skip_past_semicolon(parser: &mut Parser) -> String {
    let mut skipped = vec![];
    loop {
        match parser.next_token().token {
            Token::SemiColon | Token::EOF => return skipped.join(" "),
            t => skipped.push(t.to_string()),
        }
    }
}

/// Add the tables and foreign keys described by `statements` to `schema`,
/// in input order, so that a sequence of migrations ends with what it built:
/// `DROP TABLE`, `RENAME TABLE` and `ALTER TABLE` (adding, dropping and
/// renaming columns and constraints, `NOT NULL`, types) are applied as they
/// come. An `ALTER TABLE` on a table not yet defined is applied after the
/// rest, so a dump that adds its constraints before the tables still works.
/// Call [`Schema::resolve_foreign_keys`] once all input has been added.
pub fn add_statements(schema: &mut Schema, statements: &[Statement]) -> Vec<Warning> {
    let mut deferred = vec![];
    for stmt in statements {
        match stmt {
            Statement::CreateTable(ct) => add_create_table(schema, ct),
            Statement::AlterTable(at) => {
                if schema.find_table(&object_name(&at.name)).is_some() {
                    alter_table(schema, at);
                } else {
                    deferred.push(at);
                }
            }
            // Never deferred: a dump's `DROP TABLE IF EXISTS t` comes before
            // its `CREATE TABLE t`.
            Statement::Drop {
                object_type: ObjectType::Table,
                names,
                ..
            } => {
                for name in names {
                    if let Some(ti) = schema.find_table(&object_name(name)) {
                        schema.drop_table(ti);
                    }
                }
            }
            Statement::RenameTable(renames) => {
                for r in renames {
                    if let Some(ti) = schema.find_table(&object_name(&r.old_name)) {
                        schema.rename_table(ti, &object_name(&r.new_name));
                    }
                }
            }
            _ => {}
        }
    }

    let mut warnings = vec![];
    for at in deferred {
        let name = object_name(&at.name);
        if schema.find_table(&name).is_some() {
            alter_table(schema, at);
        } else {
            warnings.push(Warning {
                message: format!("ALTER TABLE on undefined table {name}"),
            });
        }
    }
    warnings
}

/// Apply one `ALTER TABLE` to a table that exists.
fn alter_table(schema: &mut Schema, at: &AlterTable) {
    let Some(ti) = schema.find_table(&object_name(&at.name)) else {
        return;
    };
    for op in &at.operations {
        match op {
            AlterTableOperation::AddConstraint { constraint, .. } => {
                let mut fks = vec![];
                apply_constraint(&mut schema.tables[ti], &mut fks, constraint);
                schema.foreign_keys.extend(fks);
            }
            AlterTableOperation::AddColumn { column_def, .. } => {
                let (col, fks) = column(&schema.tables[ti].name, column_def);
                schema.tables[ti].columns.push(col);
                schema.foreign_keys.extend(fks);
            }
            AlterTableOperation::DropColumn { column_names, .. } => {
                for name in column_names {
                    schema.drop_column(ti, &name.value);
                }
            }
            AlterTableOperation::DropConstraint { name, .. }
            | AlterTableOperation::DropForeignKey { name, .. } => {
                schema.drop_constraint(ti, &name.value);
            }
            AlterTableOperation::DropPrimaryKey { .. } => {
                let table = &mut schema.tables[ti];
                for c in &mut table.columns {
                    c.primary_key = false;
                }
                table.constraints.retain(|c| !c.primary_key);
            }
            AlterTableOperation::RenameColumn {
                old_column_name,
                new_column_name,
            } => {
                schema.rename_column(
                    ti,
                    &old_column_name.value,
                    &new_column_name.value,
                );
            }
            AlterTableOperation::RenameTable { table_name } => {
                let (RenameTableNameKind::To(name) | RenameTableNameKind::As(name)) =
                    table_name;
                schema.rename_table(ti, &object_name(name));
            }
            AlterTableOperation::AlterColumn { column_name, op } => {
                let Some(c) = schema.tables[ti].column_mut(&column_name.value) else {
                    continue;
                };
                match op {
                    AlterColumnOperation::SetNotNull => c.nullable = false,
                    AlterColumnOperation::DropNotNull => c.nullable = c.primary_key,
                    AlterColumnOperation::SetDataType { data_type, .. } => {
                        c.data_type = data_type.to_string();
                    }
                    _ => {}
                }
            }
            // MySQL's CHANGE (rename and redefine) and MODIFY (redefine).
            AlterTableOperation::ChangeColumn {
                old_name,
                new_name,
                data_type,
                options,
                ..
            } => {
                schema.rename_column(ti, &old_name.value, &new_name.value);
                redefine_column(
                    &mut schema.tables[ti],
                    &new_name.value,
                    data_type,
                    options,
                );
            }
            AlterTableOperation::ModifyColumn {
                col_name,
                data_type,
                options,
                ..
            } => {
                redefine_column(
                    &mut schema.tables[ti],
                    &col_name.value,
                    data_type,
                    options,
                );
            }
            _ => {}
        }
    }
}

/// Give a column a new type and options, as MySQL's `MODIFY` does: `NOT NULL`
/// must be repeated or it is lost.
fn redefine_column(
    table: &mut Table,
    name: &str,
    data_type: &DataType,
    options: &[ColumnOption],
) {
    let Some(c) = table.column_mut(name) else {
        return;
    };
    c.data_type = data_type.to_string();
    for opt in options {
        match opt {
            ColumnOption::PrimaryKey(_) => c.primary_key = true,
            ColumnOption::Unique(_) => c.unique = true,
            _ => {}
        }
    }
    c.nullable =
        !c.primary_key && !options.iter().any(|o| matches!(o, ColumnOption::NotNull));
}

fn add_create_table(schema: &mut Schema, ct: &CreateTable) {
    let mut table = Table::new(object_name(&ct.name));
    let mut fks = vec![];
    for def in &ct.columns {
        let (col, col_fks) = column(&table.name, def);
        table.columns.push(col);
        fks.extend(col_fks);
    }
    for constraint in &ct.constraints {
        apply_constraint(&mut table, &mut fks, constraint);
    }
    // Some dialects (e.g. BigQuery, Snowflake) put PRIMARY KEY in its own field.
    if let Some(pk) = &ct.primary_key
        && let Expr::Identifier(i) = pk.as_ref()
    {
        set_primary_key(&mut table, std::slice::from_ref(&i.value));
    }
    schema.upsert_table(table);
    schema.foreign_keys.extend(fks);
}

fn column(table: &str, def: &ColumnDef) -> (Column, Vec<ForeignKey>) {
    let mut col = Column {
        name: def.name.value.clone(),
        data_type: def.data_type.to_string(),
        primary_key: false,
        foreign_key: false,
        unique: false,
        nullable: true,
    };
    let mut fks = vec![];
    for opt in &def.options {
        match &opt.option {
            ColumnOption::NotNull => col.nullable = false,
            ColumnOption::PrimaryKey(_) => {
                col.primary_key = true;
                col.nullable = false;
            }
            ColumnOption::Unique(_) => col.unique = true,
            ColumnOption::ForeignKey(fk) => {
                let mut fk = foreign_key(table, fk);
                if fk.from_columns.is_empty() {
                    fk.from_columns = vec![col.name.clone()];
                }
                if fk.name.is_none() {
                    fk.name = opt.name.as_ref().map(|n| n.value.clone());
                }
                fks.push(fk);
            }
            _ => {}
        }
    }
    (col, fks)
}

fn apply_constraint(
    table: &mut Table,
    fks: &mut Vec<ForeignKey>,
    constraint: &TableConstraint,
) {
    match constraint {
        TableConstraint::PrimaryKey(pk) => {
            let columns = index_columns(&pk.columns);
            set_primary_key(table, &columns);
            if let Some(name) = &pk.name {
                table.constraints.push(NamedConstraint {
                    name: name.value.clone(),
                    primary_key: true,
                    columns,
                });
            }
        }
        TableConstraint::Unique(u) => {
            if let [only] = index_columns(&u.columns).as_slice()
                && let Some(c) = table.column_mut(only)
            {
                c.unique = true;
                if let Some(name) = &u.name {
                    table.constraints.push(NamedConstraint {
                        name: name.value.clone(),
                        primary_key: false,
                        columns: vec![only.clone()],
                    });
                }
            }
        }
        TableConstraint::ForeignKey(fk) => fks.push(foreign_key(&table.name, fk)),
        _ => {}
    }
}

fn set_primary_key(table: &mut Table, columns: &[String]) {
    for name in columns {
        if let Some(c) = table.column_mut(name) {
            c.primary_key = true;
            c.nullable = false;
        }
    }
}

fn foreign_key(table: &str, fk: &ForeignKeyConstraint) -> ForeignKey {
    ForeignKey {
        name: fk.name.as_ref().map(|n| n.value.clone()),
        from_table: table.to_string(),
        from_columns: fk.columns.iter().map(|c| c.value.clone()).collect(),
        to_table: object_name(&fk.foreign_table),
        to_columns: fk
            .referred_columns
            .iter()
            .map(|c| c.value.clone())
            .collect(),
        inferred: false,
    }
}

fn index_columns(cols: &[IndexColumn]) -> Vec<String> {
    cols.iter()
        .map(|c| match &c.column.expr {
            Expr::Identifier(i) => i.value.clone(),
            Expr::CompoundIdentifier(parts) => {
                parts.last().map(|i| i.value.clone()).unwrap_or_default()
            }
            other => other.to_string(),
        })
        .collect()
}

/// `schema.table` with quotes removed.
fn object_name(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(i) => i.value.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::{GenericDialect, MySqlDialect, PostgreSqlDialect};

    fn schema(dialect: &dyn Dialect, sql: &str) -> (Schema, Vec<Warning>) {
        let (stmts, mut warnings) = parse_statements(dialect, sql);
        let mut s = Schema::default();
        warnings.extend(add_statements(&mut s, &stmts));
        s.resolve_foreign_keys();
        (s, warnings)
    }

    #[test]
    fn preprocess_batch_separators() {
        let sql = "create table a (x int)\nGO\nselect 1\n/\n";
        assert_eq!(preprocess(sql), "create table a (x int)\n;\nselect 1\n;\n");
    }

    #[test]
    fn preprocess_mysql_delimiter() {
        let sql = "DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; END$$\nDELIMITER ;\nselect 2;\n";
        let out = preprocess(sql);
        assert_eq!(out.lines().count(), 4);
        assert!(out.contains("END;"));
        assert!(!out.contains("$$"));
    }

    #[test]
    fn oracle_constraint_states() {
        let sql = "CREATE TABLE t1 (id NUMBER(19,0) NOT NULL ENABLE, enable NUMBER,\n\
                   CONSTRAINT fk FOREIGN KEY (id) REFERENCES t2 (id) ON DELETE CASCADE ENABLE NOVALIDATE\n) ;";
        let (s, w) = schema(&sqlparser::dialect::OracleDialect {}, sql);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(s.tables[0].columns.len(), 2, "column named enable is kept");
        assert_eq!(s.foreign_keys.len(), 1);
    }

    #[test]
    fn sqlite_single_quoted_columns() {
        let sql = "create table person ('name' varchar(20) not null, 'age' integer);\n\
                   insert into person values ('a', 1);";
        let (s, w) = schema(&SQLiteDialect {}, sql);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(s.tables[0].columns[0].name, "name");
        assert!(!s.tables[0].columns[0].nullable);
    }

    #[test]
    fn recovers_from_errors_at_any_position() {
        for sql in [
            "garbage;",
            "\n  garbage here;\ncreate table b (x int);",
            "create table a (id int); garbage here; create table b (x int);",
            "create table a (id int)\n  garbage",
            "create table a (id int; create table b (x int);",
        ] {
            let (stmts, w) = parse_statements(&GenericDialect {}, sql);
            assert_eq!(w.len(), 1, "{sql:?}: {w:?}");
            assert!(stmts.len() <= 2, "{sql:?}");
        }
    }

    #[test]
    fn recovers_after_bad_statement() {
        let sql = "create table a (id int primary key);\n\
                   this is not sql;\n\
                   create table b (id int, a_id int references a);\n";
        let (s, w) = schema(&GenericDialect {}, sql);
        assert_eq!(w.len(), 1);
        assert_eq!(s.tables.len(), 2);
        assert_eq!(s.foreign_keys.len(), 1);
        assert_eq!(s.foreign_keys[0].to_columns, vec!["id"]);
    }

    #[test]
    fn table_constraints_and_alter() {
        let sql = r#"
            CREATE TABLE "public"."users" (id serial, email text NOT NULL, UNIQUE (email));
            CREATE TABLE orders (
                order_id int, user_id int NOT NULL, line int,
                PRIMARY KEY (order_id, line)
            );
            ALTER TABLE ONLY users ADD CONSTRAINT users_pkey PRIMARY KEY (id);
            ALTER TABLE orders ADD CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES public.users (id);
            INSERT INTO users VALUES (1, 'x');
        "#;
        let (s, w) = schema(&PostgreSqlDialect {}, sql);
        assert!(w.is_empty(), "{w:?}");
        let users = s.table("users").unwrap();
        assert_eq!(users.name, "public.users");
        assert_eq!(users.primary_key(), vec!["id"]);
        assert!(users.column("email").unwrap().unique);
        let orders = s.table("orders").unwrap();
        assert_eq!(orders.primary_key(), vec!["order_id", "line"]);
        let fk = &s.foreign_keys[0];
        assert_eq!(fk.name.as_deref(), Some("fk_user"));
        assert_eq!(fk.to_table, "public.users");
        assert!(!fk.is_optional(&s));
    }

    #[test]
    fn mysql_inline_index_and_fk() {
        let sql = "CREATE TABLE `child` (\n\
                     `id` int NOT NULL AUTO_INCREMENT,\n\
                     `parent_id` int DEFAULT NULL,\n\
                     PRIMARY KEY (`id`),\n\
                     KEY `parent_id` (`parent_id`),\n\
                     CONSTRAINT `fk` FOREIGN KEY (`parent_id`) REFERENCES `parent` (`id`)\n\
                   ) ENGINE=InnoDB;";
        let (s, w) = schema(&MySqlDialect {}, sql);
        assert!(w.is_empty(), "{w:?}");
        assert!(s.table("parent").unwrap().stub);
        assert!(s.foreign_keys[0].is_optional(&s));
    }

    #[test]
    fn pg_dump_noise_is_silent() {
        let sql = "\\restrict abc123\n\
                   CREATE TABLE public.a (id bigint NOT NULL);\n\
                   ALTER TABLE public.a ALTER COLUMN id ADD GENERATED BY DEFAULT AS IDENTITY (\n\
                   \x20   SEQUENCE NAME public.a_id_seq\n\
                   \x20   START WITH 1\n\
                   \x20   CACHE 1\n\
                   );\n\
                   ALTER TABLE ONLY public.a ADD CONSTRAINT a_pkey PRIMARY KEY (id);\n\
                   \\unrestrict abc123\n";
        let (s, w) = schema(&PostgreSqlDialect {}, sql);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(s.table("a").unwrap().primary_key(), vec!["id"]);

        // Anything else that fails still warns.
        let (_, w) = schema(&PostgreSqlDialect {}, "ALTER TABLE a FROBNICATE;");
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn migrations_in_order() {
        let sql = "
            CREATE TABLE parent (id int, code int);
            ALTER TABLE parent ADD CONSTRAINT parent_pkey PRIMARY KEY (id);
            ALTER TABLE parent ADD CONSTRAINT parent_code_key UNIQUE (code);
            CREATE TABLE child (id int PRIMARY KEY, parent_id int, old_col int, note text);
            ALTER TABLE child ADD CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent (id);
            CREATE TABLE scratch (id int, parent_id int REFERENCES parent);
            ALTER TABLE child RENAME COLUMN parent_id TO owner_id;
            ALTER TABLE parent RENAME TO owner;
            ALTER TABLE child DROP COLUMN old_col;
            ALTER TABLE child ALTER COLUMN note SET NOT NULL;
            ALTER TABLE child ALTER COLUMN note TYPE varchar(10);
            ALTER TABLE owner DROP CONSTRAINT parent_code_key;
            DROP TABLE scratch;
        ";
        let (s, w) = schema(&PostgreSqlDialect {}, sql);
        assert!(w.is_empty(), "{w:?}");
        let names: Vec<_> = s.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["owner", "child"]);

        let child = s.table("child").unwrap();
        assert!(child.column("old_col").is_none());
        let note = child.column("note").unwrap();
        assert!(!note.nullable);
        assert_eq!(note.data_type, "VARCHAR(10)");
        assert!(!s.table("owner").unwrap().column("code").unwrap().unique);

        assert_eq!(s.foreign_keys.len(), 1, "scratch's key went with it");
        let fk = &s.foreign_keys[0];
        assert_eq!(fk.from_columns, vec!["owner_id"]);
        assert_eq!(fk.to_table, "owner");
        assert!(child.column("owner_id").unwrap().foreign_key);

        let (s, _) = schema(
            &PostgreSqlDialect {},
            &format!("{sql} ALTER TABLE child DROP CONSTRAINT child_parent_fk;"),
        );
        assert!(s.foreign_keys.is_empty());
        let (s, _) = schema(
            &PostgreSqlDialect {},
            &format!("{sql} ALTER TABLE owner DROP CONSTRAINT parent_pkey;"),
        );
        assert!(s.table("owner").unwrap().primary_key().is_empty());
    }

    #[test]
    fn rename_keeps_schema_and_drop_comes_first() {
        let sql = "CREATE TABLE public.a (id int PRIMARY KEY);
                   ALTER TABLE public.a RENAME TO b;
                   DROP TABLE IF EXISTS c;
                   CREATE TABLE c (id int, b_id int REFERENCES public.b);";
        let (s, w) = schema(&PostgreSqlDialect {}, sql);
        assert!(w.is_empty(), "{w:?}");
        assert!(s.tables.iter().any(|t| t.name == "public.b"));
        assert!(
            s.table("c").is_some(),
            "a DROP before the CREATE drops nothing"
        );
        assert_eq!(s.foreign_keys[0].to_table, "public.b");
    }

    #[test]
    fn alter_before_create_is_applied_last() {
        let sql =
            "ALTER TABLE b ADD CONSTRAINT fk FOREIGN KEY (a_id) REFERENCES a (id);
                   CREATE TABLE a (id int PRIMARY KEY);
                   CREATE TABLE b (a_id int);
                   ALTER TABLE nowhere ADD COLUMN x int;";
        let (s, w) = schema(&PostgreSqlDialect {}, sql);
        assert_eq!(s.foreign_keys.len(), 1);
        assert_eq!(w.len(), 1);
        assert!(w[0].message.contains("undefined table nowhere"));
    }

    #[test]
    fn mysql_change_and_modify() {
        let sql = "CREATE TABLE t (id int NOT NULL, a int NOT NULL, b int);
                   ALTER TABLE t CHANGE a c bigint;
                   ALTER TABLE t MODIFY b varchar(5) NOT NULL;
                   RENAME TABLE t TO u;";
        let (s, w) = schema(&MySqlDialect {}, sql);
        assert!(w.is_empty(), "{w:?}");
        let u = s.table("u").unwrap();
        let c = u.column("c").unwrap();
        assert_eq!(c.data_type, "BIGINT");
        assert!(c.nullable, "MODIFY/CHANGE without NOT NULL drops it");
        assert!(!u.column("b").unwrap().nullable);
    }
}
