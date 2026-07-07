use std::collections::{hash_map::Entry, HashMap};
use std::fs;
use std::path::Path;

use ::scip::types::{
    occurrence, symbol_information, Index, MultiLineRange, Occurrence, PositionEncoding,
    SingleLineRange, SymbolInformation,
};
use anyhow::{bail, Context, Result};
use protobuf::Message;

use super::resolver::{Position, SourceRange};

#[derive(Debug, Clone)]
pub(crate) struct IngestedIndex {
    pub(crate) files: HashMap<String, FileOccurrences>,
    pub(crate) symbols: Vec<SymbolData>,
    pub(crate) documents: usize,
    pub(crate) occurrences: usize,
    pub(crate) definitions: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct FileOccurrences {
    pub(crate) occurrences: Vec<OccurrenceEntry>,
    pub(crate) max_line_span: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct OccurrenceEntry {
    pub(crate) range: SourceRange,
    #[allow(dead_code)]
    pub(crate) enclosing_range: Option<SourceRange>,
    pub(crate) symbol_id: usize,
    pub(crate) symbol_roles: i32,
}

#[derive(Debug, Clone)]
pub(crate) struct SymbolData {
    pub(crate) symbol: String,
    pub(crate) display_name: Option<String>,
    pub(crate) kind: Option<String>,
    pub(crate) is_local: bool,
}

pub(crate) fn read_index(path: &Path) -> Result<Index> {
    let bytes = fs::read(path).with_context(|| {
        format!(
            "substrate index missing at {}; run `moosedev index`",
            path.display()
        )
    })?;
    Index::parse_from_bytes(&bytes).with_context(|| {
        format!(
            "failed to parse substrate SCIP index at {}; run `moosedev index`",
            path.display()
        )
    })
}

pub(crate) fn ingest(index: &Index) -> Result<IngestedIndex> {
    let mut files = HashMap::new();
    let mut symbol_ids = HashMap::<String, usize>::new();
    let mut symbols = Vec::<SymbolData>::new();
    let mut occurrence_count = 0usize;
    let mut definition_count = 0usize;

    for document in &index.documents {
        validate_position_encoding(
            &document.relative_path,
            document.position_encoding.enum_value(),
        )?;
        for symbol_info in &document.symbols {
            intern_symbol(symbol_info, &mut symbol_ids, &mut symbols);
        }
    }

    for document in &index.documents {
        let mut entries = Vec::with_capacity(document.occurrences.len());
        let mut max_line_span = 0u32;

        for occurrence in &document.occurrences {
            let range = occurrence_range(occurrence)
                .with_context(|| format!("invalid SCIP range in {}", document.relative_path))?;
            let enclosing_range = occurrence_enclosing_range(occurrence).with_context(|| {
                format!("invalid SCIP enclosing_range in {}", document.relative_path)
            })?;
            let symbol_id = intern_symbol_str(&occurrence.symbol, &mut symbol_ids, &mut symbols);
            let line_span = range.end.line.saturating_sub(range.start.line);
            max_line_span = max_line_span.max(line_span);
            if is_definition_role(occurrence.symbol_roles) {
                definition_count += 1;
            }
            occurrence_count += 1;
            entries.push(OccurrenceEntry {
                range,
                enclosing_range,
                symbol_id,
                symbol_roles: occurrence.symbol_roles,
            });
        }

        entries.sort_by(|a, b| {
            range_key(&a.range)
                .cmp(&range_key(&b.range))
                .then_with(|| span_len(&a.range).cmp(&span_len(&b.range)))
                .then_with(|| {
                    symbols[a.symbol_id]
                        .symbol
                        .cmp(&symbols[b.symbol_id].symbol)
                })
        });

        files.insert(
            document.relative_path.clone(),
            FileOccurrences {
                occurrences: entries,
                max_line_span,
            },
        );
    }

    Ok(IngestedIndex {
        files,
        symbols,
        documents: index.documents.len(),
        occurrences: occurrence_count,
        definitions: definition_count,
    })
}

pub(crate) fn producer_info(index: &Index) -> (String, String) {
    index
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.tool_info.as_ref())
        .map(|tool| {
            let name = if tool.name.is_empty() {
                "unknown".to_string()
            } else {
                tool.name.clone()
            };
            let version = if tool.version.is_empty() {
                "unknown".to_string()
            } else {
                tool.version.clone()
            };
            (name, version)
        })
        .unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()))
}

pub(crate) fn is_definition_role(symbol_roles: i32) -> bool {
    symbol_roles & 1 != 0
}

fn validate_position_encoding(
    relative_path: &str,
    encoding: std::result::Result<PositionEncoding, i32>,
) -> Result<()> {
    let encoding = match encoding {
        Ok(encoding) => encoding,
        Err(value) => bail!(
            "unsupported SCIP position_encoding value {} in {}; expected UTF-8; run `moosedev index` with a UTF-8 producer",
            value,
            relative_path
        ),
    };
    match encoding {
        PositionEncoding::UTF8CodeUnitOffsetFromLineStart => Ok(()),
        PositionEncoding::UnspecifiedPositionEncoding => {
            tracing::warn!(
                path = relative_path,
                "SCIP document has unspecified position_encoding; treating as UTF-8"
            );
            Ok(())
        }
        PositionEncoding::UTF16CodeUnitOffsetFromLineStart
        | PositionEncoding::UTF32CodeUnitOffsetFromLineStart => {
            bail!(
                "unsupported SCIP position_encoding {:?} in {}; expected UTF-8; run `moosedev index` with a UTF-8 producer",
                encoding,
                relative_path
            )
        }
    }
}

fn intern_symbol(
    info: &SymbolInformation,
    symbol_ids: &mut HashMap<String, usize>,
    symbols: &mut Vec<SymbolData>,
) -> usize {
    let display_name = empty_to_none(&info.display_name);
    let kind = info.kind.enum_value().ok().and_then(|kind| match kind {
        symbol_information::Kind::UnspecifiedKind => None,
        _ => Some(format!("{kind:?}")),
    });

    match symbol_ids.entry(info.symbol.clone()) {
        Entry::Occupied(entry) => *entry.get(),
        Entry::Vacant(entry) => {
            let id = symbols.len();
            symbols.push(SymbolData {
                symbol: entry.key().clone(),
                display_name,
                kind,
                is_local: is_local_symbol(entry.key()),
            });
            entry.insert(id);
            id
        }
    }
}

fn intern_symbol_str(
    symbol: &str,
    symbol_ids: &mut HashMap<String, usize>,
    symbols: &mut Vec<SymbolData>,
) -> usize {
    match symbol_ids.entry(symbol.to_string()) {
        Entry::Occupied(entry) => *entry.get(),
        Entry::Vacant(entry) => {
            let id = symbols.len();
            symbols.push(SymbolData {
                symbol: entry.key().clone(),
                display_name: None,
                kind: None,
                is_local: is_local_symbol(entry.key()),
            });
            entry.insert(id);
            id
        }
    }
}

fn is_local_symbol(symbol: &str) -> bool {
    ::scip::symbol::is_local_symbol(symbol) || symbol.starts_with("local ")
}

fn empty_to_none(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn occurrence_range(occurrence: &Occurrence) -> Result<SourceRange> {
    match &occurrence.typed_range {
        Some(occurrence::Typed_range::SingleLineRange(range)) => single_line_range(range),
        Some(occurrence::Typed_range::MultiLineRange(range)) => multi_line_range(range),
        None => normalize_repeated_range(&occurrence.range),
        Some(_) => bail!("unsupported SCIP typed range variant"),
    }
}

fn occurrence_enclosing_range(occurrence: &Occurrence) -> Result<Option<SourceRange>> {
    let range = match &occurrence.typed_enclosing_range {
        Some(occurrence::Typed_enclosing_range::SingleLineEnclosingRange(range)) => {
            Some(single_line_range(range)?)
        }
        Some(occurrence::Typed_enclosing_range::MultiLineEnclosingRange(range)) => {
            Some(multi_line_range(range)?)
        }
        None if occurrence.enclosing_range.is_empty() => None,
        None => Some(normalize_repeated_range(&occurrence.enclosing_range)?),
        Some(_) => bail!("unsupported SCIP typed enclosing_range variant"),
    };
    Ok(range)
}

fn single_line_range(range: &SingleLineRange) -> Result<SourceRange> {
    make_range(
        range.line,
        range.start_character,
        range.line,
        range.end_character,
    )
}

fn multi_line_range(range: &MultiLineRange) -> Result<SourceRange> {
    make_range(
        range.start_line,
        range.start_character,
        range.end_line,
        range.end_character,
    )
}

fn normalize_repeated_range(range: &[i32]) -> Result<SourceRange> {
    match range {
        [line, start_col, end_col] => make_range(*line, *start_col, *line, *end_col),
        [start_line, start_col, end_line, end_col] => {
            make_range(*start_line, *start_col, *end_line, *end_col)
        }
        _ => bail!("SCIP ranges must have 3 or 4 elements, got {}", range.len()),
    }
}

fn make_range(start_line: i32, start_col: i32, end_line: i32, end_col: i32) -> Result<SourceRange> {
    if start_line < 0 || start_col < 0 || end_line < 0 || end_col < 0 {
        bail!("SCIP ranges cannot contain negative positions");
    }
    let range = SourceRange {
        start: Position {
            line: start_line as u32,
            col: start_col as u32,
        },
        end: Position {
            line: end_line as u32,
            col: end_col as u32,
        },
    };
    if compare_position(range.start, range.end) > std::cmp::Ordering::Equal {
        bail!("SCIP range start must be before or equal to end");
    }
    Ok(range)
}

fn range_key(range: &SourceRange) -> (u32, u32, u32, u32) {
    (
        range.start.line,
        range.start.col,
        range.end.line,
        range.end.col,
    )
}

fn span_len(range: &SourceRange) -> (u32, u32) {
    (
        range.end.line.saturating_sub(range.start.line),
        range.end.col.saturating_sub(range.start.col),
    )
}

fn compare_position(a: Position, b: Position) -> std::cmp::Ordering {
    (a.line, a.col).cmp(&(b.line, b.col))
}
