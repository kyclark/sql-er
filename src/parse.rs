//! Turn DDL text into a [`Schema`], skipping statements that don't parse.

use crate::model::{Column, ForeignKey, Schema, Table};
use sqlparser::ast::{
    AlterTableOperation, ColumnDef, ColumnOption, CreateTable, Expr,
    ForeignKeyConstraint, IndexColumn, ObjectName, ObjectNamePart, Statement,
    TableConstraint,
};
use sqlparser::dialect::{Dialect, SQLiteDialect};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, TokenWithSpan, Tokenizer, Whitespace};

/// A statement that was skipped, with the parser's explanation.
#[derive(Debug, Clone)]
pub struct Warning {
    pub message: String,
}

/// Rewrite client-side batch syntax that sqlparser doesn't understand into
/// plain `;` terminators, keeping line numbers intact:
/// `GO` (SQL Server/Sybase), a lone `/` (Oracle SQL*Plus), and MySQL's
/// `DELIMITER` command.
pub fn preprocess(sql: &str) -> String {
    let mut delimiter = String::from(";");
    let mut out = String::with_capacity(sql.len());
    for line in sql.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let eol = &line[body.len()..];
        let trimmed = body.trim();
        let mut words = trimmed.split_whitespace();
        if words
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
                warnings.push(Warning {
                    message: e.to_string(),
                });
                // The parser may have stopped anywhere, even past a `;`, so
                // rewind to where the statement began and skip just that one.
                // (Rebuilding the parser instead would copy the remaining
                // tokens on every error, which is quadratic.)
                while parser.index() > stmt_start {
                    parser.prev_token();
                }
                skip_past_semicolon(&mut parser);
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

fn skip_past_semicolon(parser: &mut Parser) {
    loop {
        match parser.next_token().token {
            Token::SemiColon | Token::EOF => return,
            _ => {}
        }
    }
}

/// Add the tables and foreign keys described by `statements` to `schema`.
/// `ALTER TABLE` statements are applied after all `CREATE TABLE`s so that
/// their order in the input doesn't matter. Call
/// [`Schema::resolve_foreign_keys`] once all input has been added.
pub fn add_statements(schema: &mut Schema, statements: &[Statement]) -> Vec<Warning> {
    let mut warnings = vec![];
    for stmt in statements {
        if let Statement::CreateTable(ct) = stmt {
            add_create_table(schema, ct);
        }
    }
    for stmt in statements {
        let Statement::AlterTable(at) = stmt else {
            continue;
        };
        let name = object_name(&at.name);
        let Some(ti) = schema.find_table(&name) else {
            warnings.push(Warning {
                message: format!("ALTER TABLE on undefined table {name}"),
            });
            continue;
        };
        let table_name = schema.tables[ti].name.clone();
        for op in &at.operations {
            match op {
                AlterTableOperation::AddConstraint { constraint, .. } => {
                    let mut fks = vec![];
                    apply_constraint(&mut schema.tables[ti], &mut fks, constraint);
                    schema.foreign_keys.extend(fks);
                }
                AlterTableOperation::AddColumn { column_def, .. } => {
                    let (col, fks) = column(&table_name, column_def);
                    schema.tables[ti].columns.push(col);
                    schema.foreign_keys.extend(fks);
                }
                _ => {}
            }
        }
    }
    warnings
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
            set_primary_key(table, &index_columns(&pk.columns));
        }
        TableConstraint::Unique(u) => {
            if let [only] = index_columns(&u.columns).as_slice()
                && let Some(c) = table.column_mut(only)
            {
                c.unique = true;
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
}
