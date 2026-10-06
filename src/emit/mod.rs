//! Text renderings of a [`Schema`].

pub mod dot;
pub mod mermaid;
pub mod plantuml;

use crate::model::{Column, Table};

#[derive(Debug, Clone)]
pub struct EmitOptions {
    pub title: Option<String>,
    /// Show column data types.
    pub show_types: bool,
    /// Only show primary- and foreign-key columns.
    pub keys_only: bool,
    /// GraphViz `rankdir` (LR, TB, RL, BT).
    pub rankdir: String,
}

impl Default for EmitOptions {
    fn default() -> Self {
        EmitOptions {
            title: None,
            show_types: true,
            keys_only: false,
            rankdir: "LR".into(),
        }
    }
}

impl EmitOptions {
    /// The columns of `table` to draw, with their index in the table.
    pub fn visible_columns<'a>(&self, table: &'a Table) -> Vec<(usize, &'a Column)> {
        table
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| !self.keys_only || c.primary_key || c.foreign_key)
            .collect()
    }
}
