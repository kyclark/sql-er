# sql-er

Draw entity-relationship diagrams from SQL DDL files. You don't need a database connection.

`sql-er` reads `CREATE TABLE` and `ALTER TABLE` statements, works out the
tables, columns, primary keys and foreign keys, and writes one of:

- GraphViz DOT, or an SVG/PNG/PDF/JPG rendered by GraphViz
- Mermaid `erDiagram`, which renders inline in GitHub and GitLab Markdown
- PlantUML
- JSON, the parsed model

Parsing is done by [sqlparser](https://crates.io/crates/sqlparser).
Statements it can't parse (stored procedures, vendor extensions, `INSERT`s
with odd syntax) are skipped with a warning, and the rest of the file is still
used.

## Install

```sh
cargo install --path .
```

Image output needs GraphViz on your `PATH` (`brew install graphviz`,
`apt install graphviz`). Text formats don't.

## Usage

```sh
# DOT to stdout
sql-er -d postgres schema.sql

# Format is taken from the output file's extension
sql-er -d mysql dump.sql -o schema.svg
sql-er -d sqlite app.sql -o schema.png -t "My App"

# Mermaid for a README
sql-er -d postgres schema.sql -f mermaid > docs/schema.mmd

# Large schemas: only key columns, only some tables, no orphans
sql-er -d postgres schema.sql -k -i '^billing\.' --skip-isolated -o billing.svg

# Drop the "public." prefix from table names (repeatable)
sql-er -d postgres schema.sql -s public -o schema.svg

# Schemas without declared foreign keys (e.g. MyISAM): guess from names
sql-er -d mysql legacy.sql --infer-fks -o legacy.png

# Several files (e.g. migrations), or stdin
sql-er -d postgres migrations/*.sql -o schema.pdf
pg_dump --schema-only mydb | sql-er -d postgres -o schema.svg
```

Run `sql-er --help` for all options.

### Dialects

`generic` (default), `postgres`/`pg`, `mysql`/`mariadb`, `sqlite`,
`mssql`/`sqlserver`/`sybase`, `oracle`, `snowflake`, `bigquery`, `duckdb`,
`redshift`, `ansi`.

Pick the dialect that matches the input. `generic` is permissive, but it
misses some vendor syntax, such as SQL Server `[bracketed]` names.

### Reading the diagram

- Each foreign key is a crow's-foot edge. The "many" end is at the referencing
  table and the "one" end is at the referenced table. The parent end is drawn
  as optional (`o|`) when any of the FK's columns is nullable.
- `PK`, `FK` and `U` mark primary-key, foreign-key and unique columns. Primary
  keys are bold, and a trailing `*` means `NOT NULL`.
- Tables that are referenced but never defined in the input are drawn with
  dashed borders.
- Relationships guessed by `--infer-fks` are drawn with dashed lines.

## What it understands

- Columns with `NOT NULL`, inline `PRIMARY KEY`, `UNIQUE` and
  `REFERENCES t [(col)]`. When the referenced column is omitted, the target's
  primary key is used.
- Table-level `PRIMARY KEY (...)`, `UNIQUE (...)` and `FOREIGN KEY ... REFERENCES`.
- `ALTER TABLE ... ADD CONSTRAINT` and `ADD COLUMN`, applied after all
  `CREATE TABLE`s, so the input order doesn't matter (as in `pg_dump` output).
- Schema-qualified names. `public.users` and `users` match when that's
  unambiguous. Name matching ignores case.
- Batch separators: `GO` (SQL Server/Sybase), `/` (Oracle SQL*Plus), and
  MySQL `DELIMITER`.
- Oracle `ENABLE`/`DISABLE`/`VALIDATE` constraint states, and SQLite
  single-quoted column names.

Views, indexes, triggers and comments are ignored.

## Development

```sh
cargo test
```

The tests that render SVG are skipped when GraphViz isn't installed.
