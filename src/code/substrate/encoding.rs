//! Position-encoding normalization for SCIP producer output.
//!
//! The substrate stores UTF-8 byte columns only (`scip::ingest` enforces it).
//! Some producers count columns in another unit: scip-typescript reports the
//! TypeScript compiler's UTF-16 code units and, since 0.6, declares
//! `UTF16CodeUnitOffsetFromLineStart` on each document. Rather than reject that
//! output, `moosedev index` converts it here, at the producer boundary, while
//! the source files the producer just read are on disk. Everything downstream
//! keeps seeing UTF-8 columns.
//!
//! Conversion is strict: a line or column outside the file means the source
//! changed after the producer read it, and a wrong position is worse than an
//! excluded producer, so it is an error rather than a clamp.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use protobuf::EnumOrUnknown;
use scip::types::{occurrence, Document, Index, Occurrence, PositionEncoding};

#[derive(Clone, Copy, Debug)]
enum Unit {
    Utf16,
    Utf32,
}

impl Unit {
    fn of(encoding: PositionEncoding) -> Option<Self> {
        match encoding {
            PositionEncoding::UTF16CodeUnitOffsetFromLineStart => Some(Self::Utf16),
            PositionEncoding::UTF32CodeUnitOffsetFromLineStart => Some(Self::Utf32),
            PositionEncoding::UTF8CodeUnitOffsetFromLineStart
            | PositionEncoding::UnspecifiedPositionEncoding => None,
        }
    }

    fn width(self, ch: char) -> usize {
        match self {
            Self::Utf16 => ch.len_utf16(),
            Self::Utf32 => 1,
        }
    }
}

/// Rewrite every UTF-16 or UTF-32 document of `index` to UTF-8 byte columns,
/// reading source text from the document's `text` field when the producer
/// embedded it, otherwise from `project_dir` joined with its relative path.
/// Returns how many documents were converted; documents already in UTF-8 (or
/// unspecified, which ingestion treats as UTF-8) are left untouched.
pub(crate) fn transcode_to_utf8(index: &mut Index, project_dir: &Path) -> Result<usize> {
    let mut converted = 0;
    for document in &mut index.documents {
        let Ok(encoding) = document.position_encoding.enum_value() else {
            continue; // an unknown value is reported by ingestion
        };
        let Some(unit) = Unit::of(encoding) else {
            continue;
        };
        transcode_document(document, unit, project_dir).with_context(|| {
            format!(
                "converting {encoding:?} positions in {}",
                document.relative_path
            )
        })?;
        converted += 1;
    }
    Ok(converted)
}

fn transcode_document(document: &mut Document, unit: Unit, project_dir: &Path) -> Result<()> {
    let text = if document.text.is_empty() {
        let path = project_dir.join(&document.relative_path);
        fs::read_to_string(&path).with_context(|| format!("reading source {}", path.display()))?
    } else {
        document.text.clone()
    };
    // SCIP lines are '\n'-separated; a trailing '\r' stays in the line and
    // never precedes a column that matters.
    let lines: Vec<&str> = text.split('\n').collect();
    for occurrence in &mut document.occurrences {
        transcode_occurrence(occurrence, &lines, unit)?;
    }
    document.position_encoding =
        EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
    Ok(())
}

fn transcode_occurrence(occurrence: &mut Occurrence, lines: &[&str], unit: Unit) -> Result<()> {
    transcode_repeated(&mut occurrence.range, lines, unit)?;
    transcode_repeated(&mut occurrence.enclosing_range, lines, unit)?;
    match &mut occurrence.typed_range {
        Some(occurrence::Typed_range::SingleLineRange(range)) => {
            range.start_character = column(lines, range.line, range.start_character, unit)?;
            range.end_character = column(lines, range.line, range.end_character, unit)?;
        }
        Some(occurrence::Typed_range::MultiLineRange(range)) => {
            range.start_character = column(lines, range.start_line, range.start_character, unit)?;
            range.end_character = column(lines, range.end_line, range.end_character, unit)?;
        }
        Some(_) => bail!("unsupported SCIP typed range variant"),
        None => {}
    }
    match &mut occurrence.typed_enclosing_range {
        Some(occurrence::Typed_enclosing_range::SingleLineEnclosingRange(range)) => {
            range.start_character = column(lines, range.line, range.start_character, unit)?;
            range.end_character = column(lines, range.line, range.end_character, unit)?;
        }
        Some(occurrence::Typed_enclosing_range::MultiLineEnclosingRange(range)) => {
            range.start_character = column(lines, range.start_line, range.start_character, unit)?;
            range.end_character = column(lines, range.end_line, range.end_character, unit)?;
        }
        Some(_) => bail!("unsupported SCIP typed enclosing_range variant"),
        None => {}
    }
    Ok(())
}

/// A repeated range is `[line, start, end]` or `[start_line, start, end_line, end]`;
/// any other length is left for ingestion to reject.
fn transcode_repeated(range: &mut [i32], lines: &[&str], unit: Unit) -> Result<()> {
    match range {
        [line, start, end] => {
            *start = column(lines, *line, *start, unit)?;
            *end = column(lines, *line, *end, unit)?;
        }
        [start_line, start, end_line, end] => {
            *start = column(lines, *start_line, *start, unit)?;
            *end = column(lines, *end_line, *end, unit)?;
        }
        _ => {}
    }
    Ok(())
}

/// Convert one column on one 0-based line from `unit`s to UTF-8 bytes.
fn column(lines: &[&str], line: i32, col: i32, unit: Unit) -> Result<i32> {
    let (Ok(line_index), Ok(target)) = (usize::try_from(line), usize::try_from(col)) else {
        bail!("negative SCIP position {line}:{col}");
    };
    let Some(text) = lines.get(line_index) else {
        bail!(
            "line {line} is past the end of the file ({} lines); the source changed after indexing",
            lines.len()
        );
    };
    let mut units = 0usize;
    for (byte, ch) in text.char_indices() {
        if units == target {
            return Ok(byte as i32);
        }
        units += unit.width(ch);
        if units > target {
            bail!("column {col} on line {line} falls inside a character");
        }
    }
    if units == target {
        return Ok(text.len() as i32);
    }
    bail!(
        "column {col} on line {line} is past the end of the line ({units} code units); the source changed after indexing"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use scip::types::{MultiLineRange, SingleLineRange};

    // "é" is 2 UTF-8 bytes / 1 UTF-16 unit; "😀" is 4 bytes / 2 UTF-16 units / 1 scalar.
    const SOURCE: &str = "const é = '😀';\nlet x = 1;\n";

    fn document(encoding: PositionEncoding, ranges: Vec<Vec<i32>>) -> Document {
        let mut document = Document::new();
        document.relative_path = "src/a.ts".to_string();
        document.text = SOURCE.to_string();
        document.position_encoding = EnumOrUnknown::new(encoding);
        for range in ranges {
            let mut occurrence = Occurrence::new();
            occurrence.symbol = "local 0".to_string();
            occurrence.range = range;
            document.occurrences.push(occurrence);
        }
        document
    }

    fn index_of(document: Document) -> Index {
        let mut index = Index::new();
        index.documents.push(document);
        index
    }

    #[test]
    fn utf16_columns_become_utf8_bytes() {
        // `é` at UTF-16 [6,7); the closing quote after the emoji at UTF-16 [13,14).
        let mut index = index_of(document(
            PositionEncoding::UTF16CodeUnitOffsetFromLineStart,
            vec![vec![0, 6, 7], vec![0, 13, 14], vec![0, 0, 1, 3]],
        ));
        assert_eq!(
            transcode_to_utf8(&mut index, Path::new("/nonexistent")).unwrap(),
            1
        );
        let document = &index.documents[0];
        assert_eq!(document.occurrences[0].range, vec![0, 6, 8]);
        assert_eq!(document.occurrences[1].range, vec![0, 16, 17]);
        assert_eq!(document.occurrences[2].range, vec![0, 0, 1, 3]);
        assert_eq!(
            document.position_encoding.enum_value(),
            Ok(PositionEncoding::UTF8CodeUnitOffsetFromLineStart)
        );
        assert_eq!(&SOURCE[6..8], "é");
        assert_eq!(&SOURCE[16..17], "'");
    }

    #[test]
    fn utf32_columns_count_scalars() {
        // The closing quote is scalar 12 after `const é = '😀`.
        let mut index = index_of(document(
            PositionEncoding::UTF32CodeUnitOffsetFromLineStart,
            vec![vec![0, 12, 13]],
        ));
        transcode_to_utf8(&mut index, Path::new("/nonexistent")).unwrap();
        assert_eq!(index.documents[0].occurrences[0].range, vec![0, 16, 17]);
    }

    #[test]
    fn typed_and_enclosing_ranges_are_converted() {
        let mut doc = document(PositionEncoding::UTF16CodeUnitOffsetFromLineStart, vec![]);
        let mut occurrence = Occurrence::new();
        let mut single = SingleLineRange::new();
        single.line = 0;
        single.start_character = 13;
        single.end_character = 14;
        occurrence.typed_range = Some(occurrence::Typed_range::SingleLineRange(single));
        let mut multi = MultiLineRange::new();
        multi.start_line = 0;
        multi.start_character = 6;
        multi.end_line = 1;
        multi.end_character = 3;
        occurrence.typed_enclosing_range = Some(
            occurrence::Typed_enclosing_range::MultiLineEnclosingRange(multi),
        );
        occurrence.enclosing_range = vec![0, 13, 14];
        doc.occurrences.push(occurrence);
        let mut index = index_of(doc);

        transcode_to_utf8(&mut index, Path::new("/nonexistent")).unwrap();

        let occurrence = &index.documents[0].occurrences[0];
        let Some(occurrence::Typed_range::SingleLineRange(single)) = &occurrence.typed_range else {
            panic!("single-line range kept");
        };
        assert_eq!((single.start_character, single.end_character), (16, 17));
        let Some(occurrence::Typed_enclosing_range::MultiLineEnclosingRange(multi)) =
            &occurrence.typed_enclosing_range
        else {
            panic!("multi-line enclosing range kept");
        };
        assert_eq!((multi.start_character, multi.end_character), (6, 3));
        assert_eq!(occurrence.enclosing_range, vec![0, 16, 17]);
    }

    #[test]
    fn utf8_and_unspecified_documents_are_untouched() {
        for encoding in [
            PositionEncoding::UTF8CodeUnitOffsetFromLineStart,
            PositionEncoding::UnspecifiedPositionEncoding,
        ] {
            let mut index = index_of(document(encoding, vec![vec![0, 6, 7]]));
            assert_eq!(
                transcode_to_utf8(&mut index, Path::new("/nonexistent")).unwrap(),
                0
            );
            assert_eq!(index.documents[0].occurrences[0].range, vec![0, 6, 7]);
        }
    }

    #[test]
    fn source_is_read_from_disk_when_not_embedded() {
        let dir = std::env::temp_dir().join(format!("moosedev-encoding-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.ts"), SOURCE).unwrap();
        let mut doc = document(
            PositionEncoding::UTF16CodeUnitOffsetFromLineStart,
            vec![vec![0, 13, 14]],
        );
        doc.text.clear();
        let mut index = index_of(doc);

        transcode_to_utf8(&mut index, &dir).unwrap();

        assert_eq!(index.documents[0].occurrences[0].range, vec![0, 16, 17]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn positions_outside_the_source_are_errors() {
        for (range, needle) in [
            (vec![9, 0, 1], "past the end of the file"),
            (vec![1, 0, 40], "past the end of the line"),
            (vec![0, 12, 13], "inside a character"), // UTF-16 12 is mid-emoji
        ] {
            let mut index = index_of(document(
                PositionEncoding::UTF16CodeUnitOffsetFromLineStart,
                vec![range],
            ));
            let err = transcode_to_utf8(&mut index, Path::new("/nonexistent")).unwrap_err();
            assert!(format!("{err:#}").contains(needle), "{err:#}");
        }
        let mut doc = document(PositionEncoding::UTF16CodeUnitOffsetFromLineStart, vec![]);
        doc.text.clear();
        let err = transcode_to_utf8(&mut index_of(doc), Path::new("/nonexistent")).unwrap_err();
        assert!(format!("{err:#}").contains("reading source"), "{err:#}");
    }
}
