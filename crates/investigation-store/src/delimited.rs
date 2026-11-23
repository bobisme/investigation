//! Bounded, strict reader for the core's declared delimited tables.
//!
//! The delimiter comes from the selector's declared reader, never from the
//! bytes. The whole table is checked on every read, so a ragged row anywhere
//! makes the table unreadable rather than silently shifting a column. Header
//! names and row identities are compared byte for byte with no trimming, case
//! folding or Unicode normalization. Nothing is executed or loaded.
use crate::selectors::SelectorError;

/// At most this many columns per row.
pub const MAX_COLUMNS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// RFC 4180: comma separated, `"` quoting with `""` escapes.
    Csv,
    /// IANA `text/tab-separated-values`: tab separated, no quoting.
    Tsv,
}

/// Which cell to find. `row_index` is zero-based over data rows after the
/// header. `key` is a key column header and the exact identity it must hold.
#[derive(Debug, Clone, Copy)]
pub struct CellQuery<'a> {
    pub column: &'a str,
    pub row_index: Option<u64>,
    pub key: Option<(&'a str, &'a str)>,
}

/// One field: the byte span of its content (inside enclosing quotes, escapes
/// kept) and whether it was quoted, which decides how it is unescaped.
#[derive(Debug, Clone, Copy)]
struct Field {
    start: usize,
    end: usize,
    quoted: bool,
}

/// Byte span of the selected cell's content within `bytes`.
pub fn locate(
    bytes: &[u8],
    format: Format,
    query: CellQuery<'_>,
) -> Result<(usize, usize), SelectorError> {
    std::str::from_utf8(bytes).map_err(|_| invalid("The table is not valid UTF-8."))?;
    let mut rows = Rows {
        bytes,
        format,
        position: 0,
        number: 0,
    };
    let mut header = Vec::new();
    if !rows.next_row(&mut header)? {
        return Err(invalid("The table has no header row."));
    }
    let width = header.len();
    let column = unique(bytes, &header, query.column, "The column name")?
        .ok_or_else(|| invalid("No header cell is exactly equal to `column`. Header names are compared byte for byte, with no trimming or case folding."))?;
    let key = match query.key {
        Some((name, identity)) => Some((
            unique(bytes, &header, name, "The key column name")?.ok_or_else(|| {
                invalid("No header cell is exactly equal to `key_column`. Header names are compared byte for byte.")
            })?,
            identity,
        )),
        None => None,
    };
    let mut row = Vec::new();
    let mut data_rows: u64 = 0;
    let mut by_index = None;
    let mut by_key = None;
    let mut key_matches = 0usize;
    while rows.next_row(&mut row)? {
        if row.len() != width {
            return Err(invalid(&format!(
                "Row {} has {} fields; the header has {width}. The reader refuses ragged tables.",
                rows.number,
                row.len()
            )));
        }
        let index = data_rows;
        data_rows += 1;
        let cell = row[column];
        if query.row_index == Some(index) {
            by_index = Some((index, cell));
        }
        if let Some((key_column, identity)) = key
            && equals(bytes, row[key_column], identity)
        {
            key_matches += 1;
            by_key = Some((index, cell));
        }
    }
    let chosen = match (query.row_index, key) {
        (_, Some(_)) if key_matches > 1 => {
            return Err(SelectorError::Ambiguous {
                matches: key_matches,
                what: "The row identity",
                repair: "Cite the row by row_index, or choose a key_column whose values are unique.",
            });
        }
        (_, Some(_)) if key_matches == 0 => {
            return Err(invalid(
                "No data row holds `row_identity` in `key_column`.",
            ));
        }
        (Some(index), Some(_)) => {
            let (found, cell) = by_key.expect("one key match");
            if found != index {
                return Err(invalid(&format!(
                    "row_index {index} and row_identity name different rows; the identity is on data row {found}."
                )));
            }
            cell
        }
        (None, Some(_)) => by_key.expect("one key match").1,
        (Some(index), None) => by_index
            .ok_or_else(|| {
                invalid(&format!(
                    "row_index {index} is outside the table's {data_rows} data rows (zero-based, after the header)."
                ))
            })?
            .1,
        (None, None) => return Err(invalid("Supply row_index or row_identity.")),
    };
    Ok((chosen.start, chosen.end))
}

fn invalid(message: &str) -> SelectorError {
    SelectorError::Invalid(message.to_owned())
}

/// Index of the single header cell equal to `name`, if any.
fn unique(
    bytes: &[u8],
    header: &[Field],
    name: &str,
    what: &'static str,
) -> Result<Option<usize>, SelectorError> {
    let mut found = header
        .iter()
        .enumerate()
        .filter(|(_, field)| equals(bytes, **field, name))
        .map(|(index, _)| index);
    let first = found.next();
    let more = found.count();
    if more > 0 {
        return Err(SelectorError::Ambiguous {
            matches: more + 1,
            what,
            repair: "The header repeats this name; the table cannot address the column by name.",
        });
    }
    Ok(first)
}

/// Whether the unescaped field value equals `text` exactly.
fn equals(bytes: &[u8], field: Field, text: &str) -> bool {
    let raw = &bytes[field.start..field.end];
    if !field.quoted {
        return raw == text.as_bytes();
    }
    // Inside quotes every `"` is doubled; compare without allocating.
    let mut raw = raw.iter();
    let mut text = text.as_bytes().iter();
    loop {
        match (raw.next(), text.next()) {
            (None, None) => return true,
            (Some(b'"'), Some(b'"')) => {
                if raw.next() != Some(&b'"') {
                    return false;
                }
            }
            (Some(a), Some(b)) if a == b => {}
            _ => return false,
        }
    }
}

struct Rows<'a> {
    bytes: &'a [u8],
    format: Format,
    position: usize,
    /// One-based number of the last row read, header included.
    number: u64,
}

impl Rows<'_> {
    /// Read the next row into `out`. False at the end of the input.
    fn next_row(&mut self, out: &mut Vec<Field>) -> Result<bool, SelectorError> {
        out.clear();
        if self.position >= self.bytes.len() {
            return Ok(false);
        }
        self.number += 1;
        loop {
            if out.len() == MAX_COLUMNS {
                return Err(invalid(&format!(
                    "Row {} has more than {MAX_COLUMNS} fields.",
                    self.number
                )));
            }
            let field = match self.format {
                Format::Csv if self.bytes.get(self.position) == Some(&b'"') => self.quoted()?,
                _ => self.unquoted()?,
            };
            out.push(field);
            match self.bytes.get(self.position) {
                None => return Ok(true),
                Some(b'\n') => {
                    self.position += 1;
                    return Ok(true);
                }
                Some(b'\r') if self.bytes.get(self.position + 1) == Some(&b'\n') => {
                    self.position += 2;
                    return Ok(true);
                }
                Some(&byte) if byte == self.delimiter() => self.position += 1,
                Some(_) => {
                    return Err(invalid(&format!(
                        "Row {} has a stray byte after a field at offset {}.",
                        self.number, self.position
                    )));
                }
            }
        }
    }

    const fn delimiter(&self) -> u8 {
        match self.format {
            Format::Csv => b',',
            Format::Tsv => b'\t',
        }
    }

    fn unquoted(&mut self) -> Result<Field, SelectorError> {
        let start = self.position;
        while let Some(&byte) = self.bytes.get(self.position) {
            if byte == self.delimiter() || byte == b'\n' {
                break;
            }
            if byte == b'\r' {
                if self.bytes.get(self.position + 1) == Some(&b'\n') {
                    break;
                }
                return Err(invalid(&format!(
                    "Row {} has a bare carriage return at offset {}.",
                    self.number, self.position
                )));
            }
            if byte == b'"' && self.format == Format::Csv {
                return Err(invalid(&format!(
                    "Row {} has a quote inside an unquoted field at offset {}.",
                    self.number, self.position
                )));
            }
            self.position += 1;
        }
        Ok(Field {
            start,
            end: self.position,
            quoted: false,
        })
    }

    fn quoted(&mut self) -> Result<Field, SelectorError> {
        let opening = self.position;
        self.position += 1;
        let start = self.position;
        loop {
            match self.bytes.get(self.position) {
                None => {
                    return Err(invalid(&format!(
                        "Row {} has a quoted field opened at offset {opening} that never closes.",
                        self.number
                    )));
                }
                Some(b'"') if self.bytes.get(self.position + 1) == Some(&b'"') => {
                    self.position += 2;
                }
                Some(b'"') => {
                    let end = self.position;
                    self.position += 1;
                    return Ok(Field {
                        start,
                        end,
                        quoted: true,
                    });
                }
                Some(_) => self.position += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell<'a>(
        bytes: &'a [u8],
        format: Format,
        column: &str,
        row_index: Option<u64>,
        key: Option<(&str, &str)>,
    ) -> Result<&'a str, SelectorError> {
        let (start, end) = locate(
            bytes,
            format,
            CellQuery {
                column,
                row_index,
                key,
            },
        )?;
        Ok(std::str::from_utf8(&bytes[start..end]).unwrap())
    }

    #[test]
    fn csv_cells_by_index_and_identity_keep_exact_bytes() {
        let table = b"id,note,ms\r\nr1,\"a, \"\"b\"\"\",12\r\nr2,plain,\"7\nlines\"\r\n";
        assert_eq!(cell(table, Format::Csv, "ms", Some(0), None), Ok("12"));
        assert_eq!(
            cell(table, Format::Csv, "note", None, Some(("id", "r1"))),
            Ok("a, \"\"b\"\"")
        );
        assert_eq!(
            cell(table, Format::Csv, "ms", Some(1), Some(("id", "r2"))),
            Ok("7\nlines")
        );
        assert!(matches!(
            cell(table, Format::Csv, "ms", Some(0), Some(("id", "r2"))),
            Err(SelectorError::Invalid(_))
        ));
        // A quoted header or key compares by its unescaped value.
        let quoted = b"\"k\"\"ey\",v\n\"x\"\"y\",1\n";
        assert_eq!(
            cell(quoted, Format::Csv, "v", None, Some(("k\"ey", "x\"y"))),
            Ok("1")
        );
    }

    #[test]
    fn the_declared_delimiter_is_used_and_never_guessed() {
        let tsv = b"a\tb\n1,2\t3\n";
        assert_eq!(cell(tsv, Format::Tsv, "b", Some(0), None), Ok("3"));
        // Read as CSV the same bytes have one column named "a\tb".
        assert!(matches!(
            cell(tsv, Format::Csv, "b", Some(0), None),
            Err(SelectorError::Invalid(_))
        ));
        // TSV has no quoting: a quote is an ordinary byte.
        assert_eq!(
            cell(b"q\n\"x\"\n", Format::Tsv, "q", Some(0), None),
            Ok("\"x\"")
        );
    }

    #[test]
    fn malformed_ragged_or_ambiguous_tables_are_refused() {
        for bad in [
            &b""[..],
            b"a,b\n1\n",
            b"a,b\n1,2,3\n",
            b"a\n\"open\n",
            b"a\n\"x\"y\n",
            b"a\nx\"y\n",
            b"a\nx\ry\n",
            b"a\n\xff\n",
        ] {
            assert!(
                matches!(
                    cell(bad, Format::Csv, "a", Some(0), None),
                    Err(SelectorError::Invalid(_))
                ),
                "{bad:?}"
            );
        }
        assert!(matches!(
            cell(b"a,a\n1,2\n", Format::Csv, "a", Some(0), None),
            Err(SelectorError::Ambiguous { matches: 2, .. })
        ));
        assert!(matches!(
            cell(b"k,v\nx,1\nx,2\n", Format::Csv, "v", None, Some(("k", "x"))),
            Err(SelectorError::Ambiguous { matches: 2, .. })
        ));
        // No trimming or case folding.
        assert!(matches!(
            cell(b"Name\nx\n", Format::Csv, "name", Some(0), None),
            Err(SelectorError::Invalid(_))
        ));
        assert!(matches!(
            cell(b"a\nx\n", Format::Csv, "a", Some(1), None),
            Err(SelectorError::Invalid(_))
        ));
    }

    #[test]
    fn a_final_row_without_a_line_end_and_empty_cells_are_read() {
        assert_eq!(cell(b"a,b\n1,", Format::Csv, "b", Some(0), None), Ok(""));
        assert_eq!(
            cell(b"a,b\n1,\"\"", Format::Csv, "b", Some(0), None),
            Ok("")
        );
        assert_eq!(cell(b"a\r\n\r\n", Format::Csv, "a", Some(0), None), Ok(""));
    }

    #[test]
    fn column_count_is_bounded() {
        let wide = ",".repeat(MAX_COLUMNS);
        assert!(matches!(
            cell(wide.as_bytes(), Format::Csv, "", Some(0), None),
            Err(SelectorError::Invalid(_))
        ));
    }
}
