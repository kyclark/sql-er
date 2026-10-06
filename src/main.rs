use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use regex::Regex;
use sql_er::emit::{self, EmitOptions};
use sql_er::{Schema, parse, render};
use sqlparser::dialect::{
    AnsiDialect, BigQueryDialect, Dialect, DuckDbDialect, GenericDialect, MsSqlDialect,
    MySqlDialect, OracleDialect, PostgreSqlDialect, RedshiftSqlDialect, SQLiteDialect,
    SnowflakeDialect,
};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

/// Draw entity-relationship diagrams from SQL DDL.
///
/// Reads CREATE TABLE / ALTER TABLE statements and writes a diagram of the
/// tables and their foreign keys. Statements that can't be parsed are
/// skipped with a warning. Image formats (svg, png, pdf, jpg) are rendered
/// with GraphViz, which must be installed.
#[derive(Parser, Debug)]
#[command(name = "sql-er", version)]
struct Cli {
    /// DDL files to read; use "-" or omit for stdin
    files: Vec<PathBuf>,

    /// SQL dialect of the input
    #[arg(short, long, value_enum, default_value_t = DialectArg::Generic)]
    dialect: DialectArg,

    /// Output format [default: from --output's extension, else dot]
    #[arg(short, long, value_enum)]
    format: Option<Format>,

    /// Output file [default: stdout]
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Diagram title
    #[arg(short, long)]
    title: Option<String>,

    /// Direction of the layout
    #[arg(long, value_enum, default_value_t = RankDir::LR)]
    rankdir: RankDir,

    /// GraphViz layout engine (dot, neato, fdp, sfdp, circo, twopi)
    #[arg(long, default_value = "dot")]
    layout: String,

    /// Path to the GraphViz `dot` program
    #[arg(long, env = "SQL_ER_GRAPHVIZ", default_value = "dot")]
    graphviz: PathBuf,

    /// Hide column data types
    #[arg(long)]
    no_types: bool,

    /// Only show primary- and foreign-key columns
    #[arg(short, long)]
    keys_only: bool,

    /// Only include tables whose name matches this regex (repeatable)
    #[arg(short, long, value_name = "REGEX", value_parser = Regex::new)]
    include: Vec<Regex>,

    /// Exclude tables whose name matches this regex (repeatable)
    #[arg(short = 'x', long, value_name = "REGEX", value_parser = Regex::new)]
    exclude: Vec<Regex>,

    /// Leave out tables with no relationships
    #[arg(long)]
    skip_isolated: bool,

    /// Remove this schema prefix from table names, e.g. "public" (repeatable)
    #[arg(short = 's', long, value_name = "SCHEMA")]
    strip_schema: Vec<String>,

    /// Guess undeclared foreign keys from column names (drawn dashed)
    #[arg(long)]
    infer_fks: bool,

    /// Print every skipped statement, not just a count
    #[arg(short, long, conflicts_with = "quiet")]
    verbose: bool,

    /// Don't print warnings
    #[arg(short, long)]
    quiet: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum DialectArg {
    Generic,
    Ansi,
    #[value(alias = "bq")]
    Bigquery,
    Duckdb,
    #[value(alias = "sqlserver", alias = "tsql", alias = "sybase")]
    Mssql,
    #[value(alias = "mariadb")]
    Mysql,
    Oracle,
    #[value(alias = "pg", alias = "postgresql")]
    Postgres,
    Redshift,
    Snowflake,
    Sqlite,
}

impl DialectArg {
    fn dialect(self) -> Box<dyn Dialect> {
        match self {
            DialectArg::Generic => Box::new(GenericDialect {}),
            DialectArg::Ansi => Box::new(AnsiDialect {}),
            DialectArg::Bigquery => Box::new(BigQueryDialect {}),
            DialectArg::Duckdb => Box::new(DuckDbDialect {}),
            DialectArg::Mssql => Box::new(MsSqlDialect {}),
            DialectArg::Mysql => Box::new(MySqlDialect {}),
            DialectArg::Oracle => Box::new(OracleDialect {}),
            DialectArg::Postgres => Box::new(PostgreSqlDialect {}),
            DialectArg::Redshift => Box::new(RedshiftSqlDialect {}),
            DialectArg::Snowflake => Box::new(SnowflakeDialect {}),
            DialectArg::Sqlite => Box::new(SQLiteDialect {}),
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    #[value(alias = "gv")]
    Dot,
    Svg,
    Png,
    Pdf,
    #[value(alias = "jpeg")]
    Jpg,
    #[value(alias = "mmd")]
    Mermaid,
    #[value(alias = "puml")]
    Plantuml,
    Json,
}

impl Format {
    fn from_extension(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Format::from_str(&ext, true).ok().or(match ext.as_str() {
            "mermaid" => Some(Format::Mermaid),
            "plantuml" | "pu" | "uml" => Some(Format::Plantuml),
            _ => None,
        })
    }

    /// The GraphViz `-T` value for image formats.
    fn graphviz_format(self) -> Option<&'static str> {
        match self {
            Format::Svg => Some("svg"),
            Format::Png => Some("png"),
            Format::Pdf => Some("pdf"),
            Format::Jpg => Some("jpg"),
            _ => None,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug)]
#[allow(clippy::upper_case_acronyms)]
enum RankDir {
    LR,
    TB,
    RL,
    BT,
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("sql-er: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let format = cli
        .format
        .or_else(|| cli.output.as_deref().and_then(Format::from_extension))
        .unwrap_or(Format::Dot);

    if format.graphviz_format().is_some()
        && cli.output.is_none()
        && std::io::stdout().is_terminal()
    {
        bail!(
            "refusing to write {format:?} image data to a terminal; use -o FILE or redirect stdout"
        );
    }

    let dialect = cli.dialect.dialect();
    let inputs = read_inputs(&cli.files)?;

    let mut statements = vec![];
    let mut warnings = vec![];
    for (name, sql) in &inputs {
        let (s, w) = parse::parse_statements(dialect.as_ref(), sql);
        statements.extend(s);
        warnings.extend(w.into_iter().map(|w| format!("{name}: {}", w.message)));
    }
    let mut schema = Schema::default();
    warnings.extend(
        parse::add_statements(&mut schema, &statements)
            .into_iter()
            .map(|w| w.message),
    );
    schema.resolve_foreign_keys();
    if cli.infer_fks {
        schema.infer_foreign_keys();
    }
    let collisions = schema.strip_schemas(&cli.strip_schema);
    schema.filter(&cli.include, &cli.exclude, cli.skip_isolated);

    if !cli.quiet {
        for name in &collisions {
            eprintln!(
                "warning: kept the schema on {name} because another table has the same name"
            );
        }
    }
    if !cli.quiet && !warnings.is_empty() {
        if cli.verbose {
            for w in &warnings {
                eprintln!("warning: {w}");
            }
        } else {
            eprintln!(
                "warning: skipped {} statement(s) that could not be parsed (use -v for details)",
                warnings.len()
            );
        }
    }
    if schema.tables.is_empty() {
        bail!("no tables found (is --dialect right? try -v to see parse errors)");
    }

    let opts = EmitOptions {
        title: cli.title.clone(),
        show_types: !cli.no_types,
        keys_only: cli.keys_only,
        rankdir: format!("{:?}", cli.rankdir),
    };
    let bytes = match format {
        Format::Mermaid => emit::mermaid::emit(&schema, &opts).into_bytes(),
        Format::Plantuml => emit::plantuml::emit(&schema, &opts).into_bytes(),
        Format::Json => {
            let mut s = serde_json::to_string_pretty(&schema)?;
            s.push('\n');
            s.into_bytes()
        }
        Format::Dot => emit::dot::emit(&schema, &opts).into_bytes(),
        image => render::graphviz(
            &emit::dot::emit(&schema, &opts),
            &cli.graphviz,
            &cli.layout,
            image.graphviz_format().expect("image format"),
        )?,
    };

    match &cli.output {
        Some(path) => std::fs::write(path, &bytes)
            .with_context(|| format!("could not write {}", path.display()))?,
        None => std::io::stdout().lock().write_all(&bytes)?,
    }
    Ok(())
}

/// (display name, contents) for each input.
fn read_inputs(files: &[PathBuf]) -> Result<Vec<(String, String)>> {
    let stdin = || -> Result<(String, String)> {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("could not read stdin")?;
        Ok(("<stdin>".into(), s))
    };
    if files.is_empty() {
        return Ok(vec![stdin()?]);
    }
    files
        .iter()
        .map(|f| {
            if f.as_os_str() == "-" {
                return stdin();
            }
            let bytes = std::fs::read(f)
                .with_context(|| format!("could not read {}", f.display()))?;
            // Old dumps are often Latin-1; don't fail on stray bytes.
            Ok((
                f.display().to_string(),
                String::from_utf8_lossy(&bytes).into_owned(),
            ))
        })
        .collect()
}
