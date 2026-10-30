//! Render DOT to an image by running GraphViz.

use anyhow::{Context, Result, bail};
use std::io::Write;
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

    // Write on another thread, and read stdout and stderr together
    // (wait_with_output), so that no full pipe can deadlock a large diagram:
    // reading one to the end first hangs once GraphViz fills the other.
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = dot.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));

    let output = child.wait_with_output()?;
    writer.join().expect("writer thread panicked")?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        bail!("GraphViz failed ({}): {}", output.status, stderr.trim());
    }
    if !stderr.trim().is_empty() {
        eprintln!("graphviz: {}", stderr.trim());
    }
    Ok(output.stdout)
}
