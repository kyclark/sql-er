//! Render DOT to an image by running GraphViz.

use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// Run `<graphviz> -K<layout> -T<format>` over `dot` and return its output.
pub fn graphviz(
    dot: &str,
    graphviz: &Path,
    layout: &str,
    format: &str,
) -> Result<Vec<u8>> {
    let mut child = Command::new(graphviz)
        .arg(format!("-K{layout}"))
        .arg(format!("-T{format}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "could not run GraphViz ({}). Install it (e.g. `brew install graphviz`, \
                 `apt install graphviz`), pass --graphviz <PATH>, or use -f dot",
                graphviz.display()
            )
        })?;

    // Write on another thread so a large diagram can't deadlock on full pipes.
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = dot.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));

    let mut stdout = vec![];
    child
        .stdout
        .take()
        .expect("piped stdout")
        .read_to_end(&mut stdout)?;
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("piped stderr")
        .read_to_string(&mut stderr)?;
    let status = child.wait()?;
    writer.join().expect("writer thread panicked")?;

    if !status.success() {
        bail!("GraphViz failed ({status}): {}", stderr.trim());
    }
    if !stderr.trim().is_empty() {
        eprintln!("graphviz: {}", stderr.trim());
    }
    Ok(stdout)
}
