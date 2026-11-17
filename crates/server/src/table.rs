use std::io::{self, Write};

/// Left-aligned columns sized to their widest cell.
pub fn write(out: &mut impl Write, rows: &[Vec<String>]) -> io::Result<()> {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .filter_map(|r| r.get(c))
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for r in rows {
        let mut line = String::new();
        for (c, cell) in r.iter().enumerate() {
            line.push_str(cell);
            if c + 1 < r.len() {
                line.push_str(&" ".repeat(widths[c] - cell.chars().count() + 2));
            }
        }
        writeln!(out, "{}", line.trim_end())?;
    }
    Ok(())
}

pub fn print(rows: &[Vec<String>]) {
    let _ = write(&mut io::stdout().lock(), rows);
}

pub fn row<const N: usize>(cells: [&str; N]) -> Vec<String> {
    cells.map(str::to_owned).to_vec()
}
