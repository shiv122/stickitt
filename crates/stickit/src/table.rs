//! Editing Markdown tables like a grid: which cell the caret is in, moving between cells, and
//! adding rows and columns. The note stays plain Markdown; these only compute positions and edits.

use crate::markdown::{is_table_separator, table_cells, table_pipes};
use std::ops::Range;

/// A table in a note's text.
#[derive(Debug, PartialEq)]
pub struct Grid {
    /// Each row's line, as a byte range of the text; `rows[1]` is the `|---|` separator.
    pub rows: Vec<Range<usize>>,
    /// Each row's cells (the text between dividers, trimmed), as byte ranges of the text.
    /// The separator has none.
    pub cells: Vec<Vec<Range<usize>>>,
    /// Each row's dividers (`|`), as byte offsets of the text.
    pub pipes: Vec<Vec<usize>>,
}

/// The table around `pos`, if `pos` is in one.
pub fn grid_at(text: &str, pos: usize) -> Option<Grid> {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let line_of = |p: usize| starts.partition_point(|&s| s <= p) - 1;
    let range = |i: usize| starts[i]..starts.get(i + 1).map_or(text.len(), |&n| n - 1);
    let is_row = |i: usize| table_pipes(&text[range(i)]).is_some();
    let here = line_of(pos.min(text.len()));
    if !is_row(here) {
        return None;
    }
    let mut first = here;
    while first > 0 && is_row(first - 1) {
        first -= 1;
    }
    let mut last = here;
    while last + 1 < starts.len() && is_row(last + 1) {
        last += 1;
    }
    // A table is a header, a separator, then rows; a separator lower down starts another.
    let header =
        (first..last).find(|&i| is_table_separator(&text[range(i + 1)]) && !is_table_separator(&text[range(i)]))?;
    if header > here {
        return None;
    }
    let end = (header + 2..=last).find(|&i| is_table_separator(&text[range(i)])).map_or(last, |i| i.saturating_sub(2));
    if here > end {
        return None;
    }
    let rows: Vec<Range<usize>> = (header..=end).map(range).collect();
    let mut cells = Vec::with_capacity(rows.len());
    let mut pipes = Vec::with_capacity(rows.len());
    for (k, r) in rows.iter().enumerate() {
        let line = &text[r.clone()];
        let p = table_pipes(line).unwrap_or_default();
        cells.push(if k == 1 {
            Vec::new()
        } else {
            table_cells(line, &p).into_iter().map(|c| r.start + c.start..r.start + c.end).collect()
        });
        pipes.push(p.into_iter().map(|q| r.start + q).collect());
    }
    Some(Grid { rows, cells, pipes })
}

impl Grid {
    /// The widest row's number of cells.
    pub fn columns(&self) -> usize {
        self.cells.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// The row and cell `pos` is in, or nearest to on its row (None on the separator).
    pub fn cell_of(&self, pos: usize) -> Option<(usize, usize)> {
        let row = self.rows.iter().position(|r| r.contains(&pos) || r.end == pos)?;
        let cells = &self.cells[row];
        if cells.is_empty() {
            return None;
        }
        let col = cells.iter().position(|c| pos <= c.end).unwrap_or(cells.len() - 1);
        Some((row, col))
    }

    /// Where the caret goes in a cell: at the end of its text.
    pub fn caret(&self, row: usize, col: usize) -> Option<usize> {
        self.cells.get(row)?.get(col).map(|c| c.end)
    }

    /// Keeps `pos` inside a cell's text: a position on the syntax between cells moves to the
    /// next cell (`forward`) or the previous one.
    pub fn snap(&self, pos: usize, forward: bool) -> usize {
        let Some(row) = self.rows.iter().position(|r| r.contains(&pos) || r.end == pos) else { return pos };
        let cells = &self.cells[row];
        if cells.is_empty() || cells.iter().any(|c| (c.start..=c.end).contains(&pos)) {
            return pos;
        }
        if forward {
            cells.iter().find(|c| c.start > pos).map_or_else(|| cells[cells.len() - 1].end, |c| c.start)
        } else {
            cells.iter().rev().find(|c| c.end < pos).map_or(cells[0].start, |c| c.end)
        }
    }

    /// Where a click at `pos` puts the caret: in the cell whose side of the divider it is on.
    pub fn nearest(&self, pos: usize) -> usize {
        let Some(row) = self.rows.iter().position(|r| r.contains(&pos) || r.end == pos) else { return pos };
        let cells = &self.cells[row];
        if cells.is_empty() || cells.iter().any(|c| (c.start..=c.end).contains(&pos)) {
            return pos;
        }
        let prev = cells.iter().rev().find(|c| c.end <= pos);
        let next = cells.iter().find(|c| c.start >= pos);
        match (prev, next) {
            (Some(p), Some(q)) => {
                let divider = self.pipes[row].iter().copied().find(|&d| d >= p.end && d < q.start).unwrap_or(p.end);
                if pos <= divider {
                    p.end
                } else {
                    q.start
                }
            }
            (Some(p), None) => p.end,
            (None, Some(q)) => q.start,
            (None, None) => pos,
        }
    }

    /// The rows the caret can move between (all but the separator), in order.
    fn navigable(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.rows.len()).filter(|&r| r != 1)
    }

    /// The next row below `row`, or None at the bottom.
    pub fn below(&self, row: usize) -> Option<usize> {
        self.navigable().find(|&r| r > row)
    }

    /// The next row above `row`, or None at the top.
    pub fn above(&self, row: usize) -> Option<usize> {
        self.navigable().filter(|&r| r < row).last()
    }

    /// The cell after (`forward`) or before this one, row by row; None past either end.
    pub fn step(&self, row: usize, col: usize, forward: bool) -> Option<(usize, usize)> {
        let n = self.cells[row].len();
        if forward {
            if col + 1 < n {
                return Some((row, col + 1));
            }
            self.below(row).map(|r| (r, 0))
        } else {
            if col > 0 {
                return Some((row, col - 1));
            }
            self.above(row).map(|r| (r, self.cells[r].len().saturating_sub(1)))
        }
    }

    /// Text to insert at `rows.last().end` for a new empty row.
    pub fn new_row(&self) -> String {
        format!("\n|{}", "  |".repeat(self.columns().max(1)))
    }

    /// Insertions (position, text), last first, that add an empty column at the right.
    pub fn new_column(&self) -> Vec<(usize, String)> {
        self.rows
            .iter()
            .enumerate()
            .rev()
            .map(|(k, r)| {
                let cell = if k == 1 { " --- |" } else { "  |" };
                (r.end, cell.to_string())
            })
            .collect()
    }
}

/// How a column's text sits in its cells (`:---`, `:---:`, `---:`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

impl Align {
    fn parse(cell: &str) -> Align {
        let c = cell.trim();
        match (c.starts_with(':'), c.ends_with(':') && c.len() > 1) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            _ => Align::Left,
        }
    }

    fn rule(self) -> &'static str {
        match self {
            Align::Left => "---",
            Align::Center => ":---:",
            Align::Right => "---:",
        }
    }
}

/// The alignment of each column, from a `| --- | :---: |` line.
pub fn aligns(separator: &str) -> Vec<Align> {
    let t = separator.trim().trim_start_matches('|').trim_end_matches('|');
    t.split('|').map(Align::parse).collect()
}

/// An edit from a table's row or column menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableOp {
    InsertRow { at: usize },
    DuplicateRow(usize),
    MoveRow { from: usize, to: usize },
    DeleteRow(usize),
    InsertColumn { at: usize },
    MoveColumn { from: usize, to: usize },
    AlignColumn(usize, Align),
    DeleteColumn(usize),
    DeleteTable,
}

/// A table's contents, for rewriting it after a menu action. Row 0 is the header; rows are
/// numbered as in `Grid` but without the separator (so body row 1 is `Grid` row 2).
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub rows: Vec<Vec<String>>,
    pub aligns: Vec<Align>,
}

impl Model {
    pub fn read(text: &str, grid: &Grid) -> Model {
        let rows: Vec<Vec<String>> = grid
            .cells
            .iter()
            .enumerate()
            .filter(|(k, _)| *k != 1)
            .map(|(_, cells)| cells.iter().map(|c| text[c.clone()].to_string()).collect())
            .collect();
        let n = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut aligns = grid.rows.get(1).map(|r| aligns(&text[r.clone()])).unwrap_or_default();
        aligns.resize(n, Align::Left);
        let rows = rows
            .into_iter()
            .map(|mut r| {
                r.resize(n, String::new());
                r
            })
            .collect();
        Model { rows, aligns }
    }

    pub fn columns(&self) -> usize {
        self.aligns.len()
    }

    /// Applies `op`; false if it would leave nothing (the caller deletes the table).
    pub fn apply(&mut self, op: TableOp) -> bool {
        let n = self.columns();
        match op {
            TableOp::InsertRow { at } => self.rows.insert(at.min(self.rows.len()), vec![String::new(); n]),
            TableOp::DuplicateRow(r) if r < self.rows.len() => {
                let copy = self.rows[r].clone();
                self.rows.insert(r + 1, copy);
            }
            TableOp::MoveRow { from, to } if from < self.rows.len() && to < self.rows.len() => {
                let row = self.rows.remove(from);
                self.rows.insert(to, row);
            }
            TableOp::DeleteRow(r) if r < self.rows.len() => {
                // Deleting the header promotes the first row below it.
                self.rows.remove(r);
            }
            TableOp::InsertColumn { at } => {
                let at = at.min(n);
                for row in &mut self.rows {
                    row.insert(at, String::new());
                }
                self.aligns.insert(at, Align::Left);
            }
            TableOp::MoveColumn { from, to } if from < n && to < n => {
                for row in &mut self.rows {
                    let cell = row.remove(from);
                    row.insert(to, cell);
                }
                let a = self.aligns.remove(from);
                self.aligns.insert(to, a);
            }
            TableOp::AlignColumn(c, a) if c < n => self.aligns[c] = a,
            TableOp::DeleteColumn(c) if c < n => {
                for row in &mut self.rows {
                    row.remove(c);
                }
                self.aligns.remove(c);
            }
            TableOp::DeleteTable => return false,
            _ => {}
        }
        !self.rows.is_empty() && self.columns() > 0
    }

    /// The table as Markdown (no trailing newline).
    pub fn write(&self) -> String {
        let line = |cells: Vec<&str>| format!("| {} |", cells.join(" | "));
        let mut out = vec![line(self.rows[0].iter().map(String::as_str).collect())];
        out.push(line(self.aligns.iter().map(|a| a.rule()).collect()));
        out.extend(self.rows[1..].iter().map(|r| line(r.iter().map(String::as_str).collect())));
        out.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_and_column_menu_actions() {
        let text = "| A | B |\n| --- | :---: |\n| 1 | 2 |\n| 3 | 4 |";
        let g = grid_at(text, 2).unwrap();
        let base = Model::read(text, &g);
        assert_eq!(base.aligns, [Align::Left, Align::Center]);
        let run = |op| {
            let mut m = base.clone();
            m.apply(op).then(|| m.write())
        };
        assert_eq!(
            run(TableOp::InsertRow { at: 2 }).unwrap(),
            "| A | B |\n| --- | :---: |\n| 1 | 2 |\n|  |  |\n| 3 | 4 |"
        );
        assert_eq!(
            run(TableOp::DuplicateRow(1)).unwrap(),
            "| A | B |\n| --- | :---: |\n| 1 | 2 |\n| 1 | 2 |\n| 3 | 4 |"
        );
        assert_eq!(
            run(TableOp::MoveRow { from: 2, to: 1 }).unwrap(),
            "| A | B |\n| --- | :---: |\n| 3 | 4 |\n| 1 | 2 |"
        );
        assert_eq!(
            run(TableOp::DeleteRow(0)).unwrap(),
            "| 1 | 2 |\n| --- | :---: |\n| 3 | 4 |",
            "the header is replaced"
        );
        assert_eq!(
            run(TableOp::InsertColumn { at: 1 }).unwrap(),
            "| A |  | B |\n| --- | --- | :---: |\n| 1 |  | 2 |\n| 3 |  | 4 |"
        );
        assert_eq!(
            run(TableOp::MoveColumn { from: 1, to: 0 }).unwrap(),
            "| B | A |\n| :---: | --- |\n| 2 | 1 |\n| 4 | 3 |"
        );
        assert_eq!(
            run(TableOp::AlignColumn(0, Align::Right)).unwrap(),
            "| A | B |\n| ---: | :---: |\n| 1 | 2 |\n| 3 | 4 |"
        );
        assert_eq!(run(TableOp::DeleteColumn(0)).unwrap(), "| B |\n| :---: |\n| 2 |\n| 4 |");
        assert_eq!(run(TableOp::DeleteTable), None);
        // The last column or row going takes the table with it.
        let mut one = Model::read("| A |\n| --- |", &grid_at("| A |\n| --- |", 1).unwrap());
        assert!(!one.apply(TableOp::DeleteColumn(0)));
    }

    const NOTE: &str = "# Plan\n| Deal | Stage |\n| --- | --- |\n| Acme | Won |\n|  |  |\nafter";

    fn at(text: &str, needle: &str) -> usize {
        text.find(needle).unwrap()
    }

    #[test]
    fn finds_the_table_and_cells() {
        let g = grid_at(NOTE, at(NOTE, "Acme")).unwrap();
        assert_eq!(g.rows.len(), 4);
        assert_eq!(g.columns(), 2);
        assert_eq!(&NOTE[g.cells[2][1].clone()], "Won");
        assert_eq!(g.cell_of(at(NOTE, "Won") + 1), Some((2, 1)));
        assert!(grid_at(NOTE, at(NOTE, "after")).is_none());
        assert!(grid_at(NOTE, 0).is_none());
        // Pipes without a separator aren't a table.
        assert!(grid_at("| a | b |", 3).is_none());
    }

    #[test]
    fn moves_between_cells() {
        let g = grid_at(NOTE, at(NOTE, "Deal")).unwrap();
        assert_eq!(g.step(0, 1, true), Some((2, 0)), "the separator is skipped");
        assert_eq!(g.step(2, 0, false), Some((0, 1)));
        assert_eq!(g.step(3, 1, true), None, "past the last cell");
        assert_eq!(g.below(0), Some(2));
        assert_eq!(g.above(2), Some(0));
        assert_eq!(g.caret(2, 0), Some(at(NOTE, "Acme") + 4));
        // The empty row's cells are right before their closing dividers.
        let empty = g.rows[3].start;
        assert_eq!(g.caret(3, 0), Some(empty + 3));
    }

    #[test]
    fn snaps_off_the_syntax() {
        let g = grid_at(NOTE, at(NOTE, "Acme")).unwrap();
        let row = g.rows[2].start;
        assert_eq!(g.snap(row, true), at(NOTE, "Acme"), "the leading divider");
        assert_eq!(g.snap(at(NOTE, "Acme") + 5, true), at(NOTE, "Won"));
        assert_eq!(g.snap(at(NOTE, "Acme") + 5, false), at(NOTE, "Acme") + 4);
        assert_eq!(g.snap(at(NOTE, "Acme") + 2, true), at(NOTE, "Acme") + 2, "inside a cell stays");
        // A click on a divider goes to the cell on its side.
        let divider = at(NOTE, "Acme") + 5;
        assert_eq!(g.nearest(divider), at(NOTE, "Acme") + 4);
        assert_eq!(g.nearest(divider + 1), at(NOTE, "Won"));
        assert_eq!(g.nearest(row), at(NOTE, "Acme"));
    }

    #[test]
    fn adds_rows_and_columns() {
        let g = grid_at(NOTE, at(NOTE, "Acme")).unwrap();
        let mut text = NOTE.to_string();
        text.insert_str(g.rows[3].end, &g.new_row());
        assert!(text.contains("|  |  |\n|  |  |\nafter"));
        let mut text = NOTE.to_string();
        for (pos, s) in g.new_column() {
            text.insert_str(pos, &s);
        }
        assert!(text.contains("| Deal | Stage |  |\n| --- | --- | --- |\n| Acme | Won |  |"));
        assert_eq!(grid_at(&text, at(&text, "Acme")).unwrap().columns(), 3);
    }
}
