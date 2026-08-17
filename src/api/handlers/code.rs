//! Source-aware reads for CodeEntity records.
//!
//! Everything here is a projection of the CURRENT substrate index. Nothing is
//! written, and no location is ever guessed from the file's text: definitions
//! come from `Substrate::definition_location` and bytes from
//! `Substrate::read_indexed_source_windows`, which streams a file it can prove
//! did not change during or after the producer run and retains only the
//! windows a preview will actually serve. When either cannot vouch
//! for the answer, callers get metadata plus a reindex explanation — never an
//! approximately-right preview.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::header::{HOST, ORIGIN};
use axum::http::HeaderMap;
use axum::Json;
use oxigraph::model::NamedNode;
use serde::Deserialize;

use crate::api::error::ApiError;
use crate::api::handlers::records::record_iri_for_uuid;
use crate::api::models::{RecordCodeDetail, RecordSourceResponse, SourceSpan};
use crate::code::substrate::{FileDefinition, SourceRange, SourceWindowRequest, Substrate};
use crate::graph::{asserted_project_types, first_literal, AppState, CodeTerms};

/// Lines of context kept on each side of a definition in `scope=context`.
const CONTEXT_PADDING_LINES: u32 = 12;
const CONTEXT_MAX_LINES: usize = 400;
const CONTEXT_MAX_BYTES: usize = 256 * 1024;
const FULL_MAX_LINES: usize = 20_000;
const FULL_MAX_BYTES: usize = 1024 * 1024;

/// Hard ceiling on the file this route will read into memory at all. The
/// per-scope caps shape the RESPONSE; this one bounds the work, so a giant
/// generated or vendored file cannot be turned into a memory spike by a
/// request.
const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

const NO_SUBSTRATE: &str = "No code substrate is loaded. Run `moosedev index`.";
const NO_SYMBOL: &str =
    "This entity has no substrate symbol recorded, so its source cannot be located.";
const NO_DEFINITION: &str =
    "The current index has no definition for this entity's symbol. Re-run `moosedev index`.";
const UNTRUSTED_SOURCE: &str =
    "The file on disk cannot be proven to match the indexed generation, so no source is shown. Re-run `moosedev index`.";
const DEFINITION_OUTSIDE_FILE: &str =
    "The recorded definition lies outside the current file, so no source is shown. Re-run `moosedev index`.";
const DEFINITION_BEYOND_PREVIEW: &str =
    "The definition does not fit within the preview limits for this file, so no source is shown.";

#[derive(Debug, Default, Deserialize)]
pub struct SourceQuery {
    #[serde(default)]
    pub scope: Option<String>,
}

/// `GET /api/v1/records/{uuid}/source`
pub async fn get_record_source(
    State(state): State<Arc<AppState>>,
    Path(uuid): Path<String>,
    Query(query): Query<SourceQuery>,
    headers: HeaderMap,
) -> Result<Json<RecordSourceResponse>, ApiError> {
    if !source_request_is_trusted(&headers) {
        return Err(ApiError::forbidden(
            "source previews are not readable cross-origin",
        ));
    }
    let scope = match query.scope.as_deref() {
        None | Some("context") => Scope::Context,
        Some("full") => Scope::Full,
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "scope must be \"context\" or \"full\", got {other:?}"
            )));
        }
    };
    if uuid.is_empty() || uuid.contains('/') {
        return Err(ApiError::bad_request("invalid record uuid"));
    }
    let iri = record_iri_for_uuid(&state, &uuid)
        .ok_or_else(|| ApiError::not_found(format!("record {uuid:?} not found")))?;
    let terms = CodeTerms::resolve(&state)?;
    if !is_code_entity(&state, &terms, &iri) {
        return Err(ApiError::bad_request(format!(
            "record {uuid:?} is not a CodeEntity"
        )));
    }

    let symbol = first_literal(&state.store, &iri, &terms.has_substrate_symbol);
    let located = locate(&state, symbol.as_deref());
    let path = match located.availability {
        Availability::Unavailable(reason) => return Err(ApiError::unavailable(reason)),
        Availability::Ready { path } => path,
    };
    let definition = located
        .definition
        .ok_or_else(|| ApiError::internal("servable source with no located definition"))?
        .range;
    // For an indexed symbol this is the first read of the file: availability
    // was decided from metadata alone. A syntactic identity is the exception —
    // locating it already cost a (bounded, mtime-cached) parse, because its
    // declaration range exists nowhere but the file itself.
    // The read uses the SAME pinned generation that produced the definition,
    // so coordinates and bytes can never come from different indexes. It streams
    // and keeps only these windows, so a multi-megabyte file previewed 25 lines
    // at a time costs 25 lines.
    let source = located
        .substrate
        .as_deref()
        .and_then(|substrate| EntitySource::read(substrate, path.clone(), definition, scope))
        .ok_or_else(|| ApiError::unavailable(format!("{UNTRUSTED_SOURCE} ({path})")))?;
    render_source(&source, scope)
        .map(Json)
        .map_err(ApiError::unavailable)
}

/// The CodeEntity block of a record detail response, or `None` when the record
/// is not a CodeEntity.
pub(crate) fn code_detail(state: &AppState, iri: &str) -> Option<RecordCodeDetail> {
    let terms = CodeTerms::resolve(state).ok()?;
    if !is_code_entity(state, &terms, iri) {
        return None;
    }
    let symbol = first_literal(&state.store, iri, &terms.has_substrate_symbol);
    let located = locate(state, symbol.as_deref());
    let (source_available, source_unavailable_reason, source_path) = match located.availability {
        Availability::Ready { path } => (true, None, Some(path)),
        Availability::Unavailable(reason) => (
            false,
            Some(reason),
            located
                .definition
                .as_ref()
                .map(|found| found.entry.file.clone()),
        ),
    };

    Some(RecordCodeDetail {
        symbol,
        name: first_literal(&state.store, iri, &terms.has_code_name),
        entity_kind: first_literal(&state.store, iri, &terms.has_entity_kind),
        logical_path: first_literal(&state.store, iri, &terms.has_logical_path),
        defined_in_path: first_literal(&state.store, iri, &terms.defined_in_path),
        signature: located
            .definition
            .as_ref()
            .and_then(|found| found.entry.signature.clone()),
        source_path,
        definition: located.definition.map(|found| public_span(found.range)),
        source_available,
        source_unavailable_reason,
        substrate_stale: located.substrate_stale,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Context,
    Full,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Scope::Context => "context",
            Scope::Full => "full",
        }
    }

    fn caps(self) -> (usize, usize) {
        match self {
            Scope::Context => (CONTEXT_MAX_LINES, CONTEXT_MAX_BYTES),
            Scope::Full => (FULL_MAX_LINES, FULL_MAX_BYTES),
        }
    }
}

/// Trusted source for one entity: only the SLICES a preview can serve, never
/// the file they came from.
///
/// Two content windows are kept because `clip` keeps a prefix: the padded window
/// is what a preview normally shows, and the definition-anchored one is what
/// replaces it when oversized leading context would crowd the definition out.
/// The definition's last line is held on its own so its end column can always be
/// validated, even for a declaration far longer than any preview.
struct EntitySource {
    path: String,
    definition: SourceRange,
    /// Lines in the whole file, so a definition outside it is still detectable.
    total_lines: usize,
    window: Vec<String>,
    window_first: usize,
    from_definition: Vec<String>,
    definition_end_line: Option<String>,
}

impl EntitySource {
    /// Stream the windows a preview needs. The file is never materialized;
    /// `None` when the substrate cannot prove it.
    fn read(
        substrate: &Substrate,
        path: String,
        definition: SourceRange,
        scope: Scope,
    ) -> Option<Self> {
        let requests = window_requests(definition, scope);
        let (windows, total_lines) = substrate.read_indexed_source_windows(&path, &requests)?;
        Some(Self::from_windows(
            path,
            definition,
            scope,
            windows,
            total_lines,
        ))
    }

    /// Assemble from windows laid out by [`window_requests`], in that order.
    fn from_windows(
        path: String,
        definition: SourceRange,
        scope: Scope,
        windows: Vec<Vec<String>>,
        total_lines: usize,
    ) -> Self {
        let mut windows = windows.into_iter();
        Self {
            window_first: window_first_line(definition, scope),
            path,
            definition,
            total_lines,
            window: windows.next().unwrap_or_default(),
            from_definition: windows.next().unwrap_or_default(),
            definition_end_line: windows.next().and_then(|mut lines| lines.pop()),
        }
    }
}

#[cfg(test)]
impl EntitySource {
    /// Slice the SAME windows the substrate would stream, from text already
    /// held in memory. The serving path never materializes a file; this exists
    /// for callers that legitimately already have one (tests).
    fn from_full_text(path: &str, definition: SourceRange, text: &str, scope: Scope) -> Self {
        let windows = window_requests(definition, scope)
            .iter()
            .map(|request| {
                let mut held = 0usize;
                let mut kept = Vec::new();
                for (index, line) in text.lines().enumerate() {
                    if !request.wants(index, held) {
                        continue;
                    }
                    held += line.len();
                    kept.push(line.to_string());
                }
                kept
            })
            .collect();
        Self::from_windows(
            path.to_string(),
            definition,
            scope,
            windows,
            text.lines().count(),
        )
    }
}

/// The windows a preview needs, in the order [`EntitySource::from_windows`]
/// unpacks them: the padded preview window, the definition-anchored window
/// `clip` falls back to when leading context crowds the definition out, and the
/// definition's last line so its end column can always be validated.
///
/// Defined once so the streamed windows and the in-memory test windows cannot
/// come to describe different things.
fn window_requests(definition: SourceRange, scope: Scope) -> [SourceWindowRequest; 3] {
    let (max_lines, max_bytes) = scope.caps();
    [
        SourceWindowRequest {
            first_line: window_first_line(definition, scope),
            max_lines,
            max_bytes,
        },
        SourceWindowRequest {
            first_line: definition.start.line as usize,
            max_lines,
            max_bytes,
        },
        SourceWindowRequest {
            first_line: definition.end.line as usize,
            max_lines: 1,
            max_bytes: 0,
        },
    ]
}

/// First line a scope's preview window starts at — known from the definition
/// alone, so the window can be requested before anything is read.
fn window_first_line(definition: SourceRange, scope: Scope) -> usize {
    match scope {
        Scope::Full => 0,
        Scope::Context => definition.start.line.saturating_sub(CONTEXT_PADDING_LINES) as usize,
    }
}

/// Whether this entity's source can be served, decided from file metadata
/// alone so that describing a record never costs a file read.
enum Availability {
    Ready { path: String },
    Unavailable(String),
}

/// What one substrate lookup could establish about an entity, without reading
/// any source.
struct Located {
    /// Where the entity is defined, when the substrate knows.
    definition: Option<FileDefinition>,
    availability: Availability,
    substrate_stale: bool,
    /// The exact generation that produced the location above. The read MUST
    /// use this same snapshot: re-fetching would let an index published in
    /// between pair one generation's coordinates with another's bytes.
    substrate: Option<std::sync::Arc<crate::code::substrate::Substrate>>,
}

impl Located {
    fn nothing(reason: &str) -> Self {
        Self {
            definition: None,
            availability: Availability::Unavailable(reason.to_string()),
            substrate_stale: false,
            substrate: None,
        }
    }
}

/// Resolve a symbol to its definition and decide whether its source may be
/// served. The definition is reported even when the source is not, so a record
/// page can still show WHERE something lives while explaining why it cannot
/// show WHAT is there.
fn locate(state: &AppState, symbol: Option<&str>) -> Located {
    let Some(symbol) = symbol else {
        return Located::nothing(NO_SYMBOL);
    };
    let Some(substrate) = state.substrate() else {
        return Located::nothing(NO_SUBSTRATE);
    };
    let substrate_stale = substrate.is_stale();
    let Some(found) = substrate.definition_location(symbol) else {
        return Located {
            substrate_stale,
            substrate: Some(substrate),
            ..Located::nothing(NO_DEFINITION)
        };
    };
    let availability = match substrate.indexed_source_len(&found.entry.file) {
        None => Availability::Unavailable(format!("{UNTRUSTED_SOURCE} ({})", found.entry.file)),
        Some(bytes) if bytes > MAX_READ_BYTES => Availability::Unavailable(format!(
            "{} is {bytes} bytes, larger than the {MAX_READ_BYTES}-byte preview ceiling.",
            found.entry.file
        )),
        Some(_) => Availability::Ready {
            path: found.entry.file.clone(),
        },
    };
    Located {
        definition: Some(found),
        availability,
        substrate_stale,
        substrate: Some(substrate),
    }
}

/// `Err` when the definition cannot honestly be shown. Clamping instead would
/// render an arbitrary slice with a highlight pointing at nothing, which is a
/// lie about where the entity is.
fn render_source(
    source: &EntitySource,
    scope: Scope,
) -> Result<RecordSourceResponse, &'static str> {
    let total_lines = source.total_lines;
    // Probe only the two lines the span actually names. Both were retained for
    // exactly this, so validating a span never depends on holding the file.
    let definition = source.definition;
    let line_at = |line: u32| -> Option<&str> {
        if line == definition.start.line {
            source.from_definition.first().map(String::as_str)
        } else if line == definition.end.line {
            source.definition_end_line.as_deref()
        } else {
            None
        }
    };
    if !definition_fits(definition, total_lines, line_at) {
        return Err(DEFINITION_OUTSIDE_FILE);
    }
    let last = match scope {
        Scope::Full => total_lines.saturating_sub(1),
        Scope::Context => (definition.end.line.saturating_add(CONTEXT_PADDING_LINES) as usize)
            .min(total_lines.saturating_sub(1)),
    };
    let mut first = source.window_first.min(last);
    let (max_lines, max_bytes) = scope.caps();
    // Trim a retained window to the scope's last line. The window was requested
    // with the scope's line cap, so anything missing was either past EOF or past
    // a cap — `capped < wanted` distinguishes the truncation that matters.
    let (window, dropped_lines) = take_window(&source.window, first, last, max_lines);
    let (mut text, mut kept, mut truncated) = clip(&window, max_lines, max_bytes);
    truncated |= dropped_lines;

    // Leading padding must never crowd out the thing being previewed. `clip`
    // keeps a prefix, so a few oversized lines BEFORE the definition can eat
    // the whole budget and return nothing but context — with the advertised
    // definition span pointing outside the response. Drop the padding and
    // re-clip from the definition itself.
    let definition_line = definition.start.line as usize;
    if definition_line >= first + kept {
        first = definition_line;
        let (window, _) = take_window(&source.from_definition, first, last, max_lines);
        (text, kept, _) = clip(&window, max_lines, max_bytes);
        truncated = true;
    }

    // Line-level presence is not enough, and neither is the start alone:
    // `clip` can byte-truncate the definition's line or cut the window before
    // the definition ENDS. Either way the advertised span would describe text
    // the response does not contain, so validate the whole span against what
    // is actually being returned.
    if !span_is_inside(definition, &text, first, kept) {
        return Err(DEFINITION_BEYOND_PREVIEW);
    }

    Ok(RecordSourceResponse {
        path: source.path.clone(),
        scope: scope.label().to_string(),
        start_line: first as u32 + 1,
        // `first` is 0-based, so `first + kept` is the 1-based last line kept.
        end_line: (first + kept) as u32,
        total_lines: total_lines as u32,
        truncated,
        definition: Some(public_span(definition)),
        text,
    })
}

/// Trim a retained window to the scope's last line. The window was requested
/// with the scope's line cap, so anything missing was either past EOF or past a
/// cap — `capped < wanted` distinguishes the truncation worth reporting.
fn take_window(lines: &[String], first: usize, last: usize, max_lines: usize) -> (Vec<&str>, bool) {
    let wanted = (last + 1).saturating_sub(first);
    let capped = wanted.min(max_lines);
    (
        lines.iter().take(capped).map(String::as_str).collect(),
        capped < wanted,
    )
}

/// Whether the whole definition span lies inside the clipped text, which
/// starts at 0-based line `first` and holds `kept` lines.
fn span_is_inside(definition: SourceRange, text: &str, first: usize, kept: usize) -> bool {
    let returned = text.split('\n').collect::<Vec<_>>();
    let within = |line: u32, col: u32| {
        let Some(index) = (line as usize).checked_sub(first) else {
            return false;
        };
        match returned.get(index) {
            Some(content) => (col as usize) <= content.len(),
            // The exclusive end may sit at the first line past the window,
            // but only at its start — there are no bytes of it to be missing.
            None => index == kept && col == 0,
        }
    };
    within(definition.start.line, definition.start.col)
        && within(definition.end.line, definition.end.col)
}

/// Whether a definition range is a coherent span inside this file.
///
/// The whole range is validated, not just its start: a producer range whose
/// end runs past EOF — or backwards — would otherwise be advertised verbatim
/// beside a preview it does not describe. The end is EXCLUSIVE, so it may sit
/// one line past the last, which is how a declaration ending at EOF is spelled.
fn definition_fits<'a>(
    definition: SourceRange,
    total_lines: usize,
    line_at: impl Fn(u32) -> Option<&'a str>,
) -> bool {
    let start = definition.start;
    let end = definition.end;
    if (start.line as usize) >= total_lines || (end.line as usize) > total_lines {
        return false;
    }
    if (end.line, end.col) < (start.line, start.col) {
        return false;
    }
    // Columns are UTF-8 BYTE offsets, and SCIP ingestion validates ordering
    // only — never the coordinates against the file. An offset past the line's
    // end, or splitting a character, would be published as a span describing
    // text that does not exist.
    let column_fits = |line: u32, col: u32| match line_at(line) {
        Some(text) => (col as usize) <= text.len() && text.is_char_boundary(col as usize),
        // The exclusive end may sit one line past the last, but only at its
        // start: there is no column to be inside.
        None => col == 0,
    };
    column_fits(start.line, start.col) && column_fits(end.line, end.col)
}

/// Drop WHOLE lines to meet the caps, so the reported line range always
/// matches the text a client renders. Whole-line clipping is also what keeps
/// the payload UTF-8 valid without a boundary search.
fn clip(lines: &[&str], max_lines: usize, max_bytes: usize) -> (String, usize, bool) {
    if lines.is_empty() {
        return (String::new(), 0, false);
    }
    let mut kept = lines.len().min(max_lines);
    let mut truncated = kept < lines.len();
    // `join("\n")` emits kept-1 separators, not one per line. Counting one
    // each overstates the payload and drops a final line that would have fit.
    let joined = |kept: usize, content: usize| content + kept.saturating_sub(1);
    let mut content: usize = lines[..kept].iter().map(|line| line.len()).sum();
    while kept > 1 && joined(kept, content) > max_bytes {
        kept -= 1;
        content -= lines[kept].len();
        truncated = true;
    }
    let mut text = lines[..kept].join("\n");
    // Whole-line clipping cannot bring a SINGLE oversized line (minified or
    // generated source) under the cap, so cut that one at a character boundary
    // rather than return a payload past the advertised limit. `kept` still
    // describes the line range, so the reported numbering stays honest.
    if text.len() > max_bytes {
        let mut end = max_bytes;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        truncated = true;
    }
    (text, kept, truncated)
}

/// Substrate ranges are 0-based; every public coordinate is 1-based.
fn public_span(range: SourceRange) -> SourceSpan {
    SourceSpan {
        start_line: range.start.line.saturating_add(1),
        start_col: range.start.col.saturating_add(1),
        end_line: range.end.line.saturating_add(1),
        end_col: range.end.col.saturating_add(1),
    }
}

fn is_code_entity(state: &AppState, terms: &CodeTerms, iri: &str) -> bool {
    NamedNode::new(iri).is_ok_and(|node| {
        asserted_project_types(state, &node)
            .iter()
            .any(|class| class == &terms.code_entity_class)
    })
}

/// Source text is the one payload where the permissive CORS layer would be a
/// real exposure: any page a developer visits could otherwise read their
/// working tree from a localhost daemon.
///
/// Comparing `Origin` to `Host` alone is NOT enough, because both are
/// caller-controlled. A page served from a hostname whose DNS is rebound to
/// 127.0.0.1 sends matching attacker-chosen values and would pass such a test.
///
/// So the request must also address this machine by ADDRESS rather than by DNS
/// name. A rebinding page necessarily arrives under its own domain name — a
/// name is the one thing the technique cannot do without — while the workbench
/// is reached at `localhost` or a literal address. That, plus `Origin` equal to
/// `Host`, is the whole rule.
/// Note the ORDER of the two checks. A rebinding fetch is same-origin as far
/// as the browser is concerned (page and request share `rebind.example`), so
/// no `Origin` header is sent at all — the address-literal rule has to apply
/// whenever a `Host` is present, not only when an `Origin` accompanies it.
/// A request with no `Host` is not a browser request; every browser sends one.
fn source_request_is_trusted(headers: &HeaderMap) -> bool {
    let host = headers.get(HOST).and_then(|value| value.to_str().ok());
    if host.is_some_and(|host| !authority_is_address_literal(host)) {
        return false;
    }
    match headers.get(ORIGIN).and_then(|value| value.to_str().ok()) {
        // Local non-browser callers send no Origin.
        None => true,
        // A browser sending Origin must be on this exact authority, which
        // means it must have sent a Host to compare against.
        Some(origin) => host.is_some_and(|host| {
            origin
                .strip_prefix("http://")
                .or_else(|| origin.strip_prefix("https://"))
                .is_some_and(|authority| authority == host)
        }),
    }
}

/// True when an authority names its host by address (or the unrebindable
/// `localhost`) rather than by a DNS name.
fn authority_is_address_literal(authority: &str) -> bool {
    let (host, _) = split_authority(authority);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok()
}

/// Split `host[:port]`, tolerating the colons inside a bracketed IPv6 literal.
fn split_authority(authority: &str) -> (&str, Option<u16>) {
    match authority.rsplit_once(':') {
        Some((host, port)) if !port.contains(']') => (host, port.parse().ok()),
        _ => (authority, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn trusted_authorities_name_the_host_by_address_not_by_dns_name() {
        assert!(authority_is_address_literal("127.0.0.1:7474"));
        assert!(authority_is_address_literal("localhost:7474"));
        assert!(authority_is_address_literal("LocalHost:7474"));
        assert!(authority_is_address_literal("[::1]:7474"));
        assert!(authority_is_address_literal("127.0.0.1"));
        // A LAN address the operator deliberately bound is still a literal.
        assert!(authority_is_address_literal("192.168.1.5:7474"));

        // A DOMAIN NAME is what a DNS-rebinding page must arrive under, and it
        // is refused however the name resolves.
        assert!(!authority_is_address_literal("rebind.example:7474"));
        assert!(!authority_is_address_literal("localhost.evil.example:7474"));
        assert!(!authority_is_address_literal("evil.example"));
    }

    #[test]
    fn origin_policy_allows_same_origin_and_non_browser_callers() {
        assert!(source_request_is_trusted(&headers(&[(
            "host",
            "127.0.0.1:7474"
        )])));
        assert!(source_request_is_trusted(&headers(&[
            ("host", "127.0.0.1:7474"),
            ("origin", "http://127.0.0.1:7474"),
        ])));
        assert!(!source_request_is_trusted(&headers(&[
            ("host", "127.0.0.1:7474"),
            ("origin", "http://evil.example"),
        ])));
        // A different port on the same host is still a different origin.
        assert!(!source_request_is_trusted(&headers(&[
            ("host", "127.0.0.1:7474"),
            ("origin", "http://127.0.0.1:9999"),
        ])));
        // Opaque origins (sandboxed iframes, some file:// pages) are refused.
        assert!(!source_request_is_trusted(&headers(&[
            ("host", "127.0.0.1:7474"),
            ("origin", "null"),
        ])));
        // The rebinding case: Origin and Host agree, and both are the
        // attacker's. Origin/Host equality alone would have let this through.
        assert!(!source_request_is_trusted(&headers(&[
            ("host", "rebind.example:7474"),
            ("origin", "http://rebind.example:7474"),
        ])));
        // And the shape it ACTUALLY takes: a rebound page's fetch is
        // same-origin to the browser, so it carries no Origin at all.
        assert!(!source_request_is_trusted(&headers(&[(
            "host",
            "rebind.example:7474"
        )])));
        // A caller with no Host is not a browser — every browser sends one.
        assert!(source_request_is_trusted(&headers(&[])));
        // ...but one claiming an Origin without a Host cannot be checked.
        assert!(!source_request_is_trusted(&headers(&[(
            "origin",
            "http://evil.example"
        )])));
    }

    #[test]
    fn clip_drops_whole_lines_to_meet_caps() {
        let lines = vec!["one", "two", "three", "four"];
        let (text, kept, truncated) = clip(&lines, 2, usize::MAX);
        assert_eq!(text, "one\ntwo");
        assert_eq!(kept, 2);
        assert!(truncated);

        // "one\n" + "two\n" = 8 bytes; a 6-byte budget keeps only the first.
        let (text, kept, truncated) = clip(&lines, 4, 6);
        assert_eq!(text, "one");
        assert_eq!(kept, 1);
        assert!(truncated);

        let (text, kept, truncated) = clip(&lines, 4, usize::MAX);
        assert_eq!(text, "one\ntwo\nthree\nfour");
        assert_eq!(kept, 4);
        assert!(!truncated);
    }

    #[test]
    fn clip_truncates_a_single_line_that_busts_the_budget_alone() {
        // Whole-line clipping bottoms out at one line, so a minified or
        // generated line must still be cut to honor the advertised cap.
        let huge = "x".repeat(5_000);
        let lines = vec![huge.as_str(), "after"];
        let (text, kept, truncated) = clip(&lines, 4, 1_000);
        assert_eq!(text.len(), 1_000);
        assert_eq!(kept, 1);
        assert!(truncated);
    }

    #[test]
    fn clip_cuts_an_oversized_line_on_a_character_boundary() {
        // A budget landing mid-character must round DOWN, never split one.
        let huge = "é".repeat(1_000);
        let lines = vec![huge.as_str()];
        let (text, _, truncated) = clip(&lines, 1, 101);
        assert!(truncated);
        assert_eq!(text.len(), 100, "must round down to a boundary");
        assert!(text.chars().all(|c| c == 'é'));
    }

    #[test]
    fn oversized_leading_padding_never_clips_away_the_definition() {
        // Twelve lines of padding precede the definition, and here they are
        // huge. Keeping the window's prefix would spend the whole budget on
        // context and return a preview that does not contain the definition
        // the response claims to highlight.
        let filler = "x".repeat(40_000);
        let mut lines: Vec<String> = (0..30).map(|_| filler.clone()).collect();
        lines[30 - 1] = "pub fn build_server() {}".to_string();
        let definition = SourceRange {
            start: crate::code::substrate::Position { line: 29, col: 7 },
            end: crate::code::substrate::Position { line: 29, col: 19 },
        };
        let source = EntitySource::from_full_text(
            "src/runtime.rs",
            definition,
            &lines.join("\n"),
            Scope::Context,
        );

        let rendered = render_source(&source, Scope::Context).expect("rendered");
        assert!(rendered.truncated);
        assert!(
            rendered.text.contains("pub fn build_server() {}"),
            "the definition must survive clipping"
        );
        // The reported range must still describe the text returned.
        let definition_line = rendered.definition.expect("definition").start_line;
        assert!(
            (rendered.start_line..=rendered.end_line).contains(&definition_line),
            "definition line {definition_line} outside {}..={}",
            rendered.start_line,
            rendered.end_line
        );
    }

    #[test]
    fn a_definition_ending_past_the_clipped_window_is_refused() {
        // The declaration STARTS inside the window but runs past the line cap,
        // so the advertised end would point beyond the returned text.
        let text = (0..1_000)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let range = SourceRange {
            start: crate::code::substrate::Position { line: 0, col: 0 },
            end: crate::code::substrate::Position { line: 900, col: 4 },
        };

        // `full` keeps every line, so the whole span is present.
        let source = EntitySource::from_full_text("src/wide.rs", range, &text, Scope::Full);
        let full = render_source(&source, Scope::Full).expect("rendered");
        let definition = full.definition.expect("definition");
        assert!(definition.end_line <= full.end_line);
        let source = EntitySource::from_full_text("src/wide.rs", range, &text, Scope::Context);

        // `context` is capped at 400 lines, so the span's end falls outside
        // the returned text. Refusing is the honest answer: advertising a
        // clamped end would describe a declaration the response does not hold.
        assert_eq!(
            render_source(&source, Scope::Context),
            Err(DEFINITION_BEYOND_PREVIEW)
        );
    }

    #[test]
    fn clip_uses_the_separators_join_actually_emits() {
        // "one\ntwo" is 7 bytes: 3 + 3 + ONE separator. Counting a newline per
        // line would total 8 and drop "two" despite it fitting.
        let lines = vec!["one", "two"];
        let (text, kept, truncated) = clip(&lines, 4, 7);
        assert_eq!(text, "one\ntwo");
        assert_eq!(kept, 2);
        assert!(!truncated);
    }

    #[test]
    fn a_definition_past_the_byte_cap_on_one_line_is_refused() {
        // A single line longer than the budget gets byte-truncated, so a
        // definition late on that line is not in the returned text even though
        // its LINE is. Returning it would advertise a span past the payload.
        let mut line = "x".repeat(300_000);
        line.push_str("pub fn build_server() {}");
        let definition_col = 300_000u32;
        let source = EntitySource::from_full_text(
            "src/generated.rs",
            SourceRange {
                start: crate::code::substrate::Position {
                    line: 0,
                    col: definition_col,
                },
                end: crate::code::substrate::Position {
                    line: 0,
                    col: definition_col + 12,
                },
            },
            &line,
            Scope::Context,
        );

        assert_eq!(
            render_source(&source, Scope::Context),
            Err(DEFINITION_BEYOND_PREVIEW)
        );
    }

    #[test]
    fn a_definition_outside_the_file_is_refused_not_clamped() {
        let range = SourceRange {
            start: crate::code::substrate::Position { line: 99, col: 0 },
            end: crate::code::substrate::Position { line: 99, col: 4 },
        };
        let text = "one\ntwo\nthree";
        // Clamping would render an arbitrary slice with a highlight pointing
        // at nothing; the honest answer is that this cannot be shown.
        assert_eq!(
            render_source(
                &EntitySource::from_full_text("src/lib.rs", range, text, Scope::Context),
                Scope::Context
            ),
            Err(DEFINITION_OUTSIDE_FILE)
        );
        assert_eq!(
            render_source(
                &EntitySource::from_full_text("src/lib.rs", range, text, Scope::Full),
                Scope::Full
            ),
            Err(DEFINITION_OUTSIDE_FILE)
        );
    }

    #[test]
    fn definition_ranges_are_validated_end_to_end_not_just_at_the_start() {
        let position = |line, col| crate::code::substrate::Position { line, col };
        let span = |start, end| SourceRange { start, end };
        // "twö" is 4 bytes: t, w, then a 2-byte ö.
        let lines = ["one", "tw\u{f6}", "three"];
        let file = |index: u32| lines.get(index as usize).copied();
        let total = lines.len();

        // A whole coherent range inside the file.
        assert!(definition_fits(
            span(position(0, 0), position(1, 4)),
            total,
            file
        ));
        // The end is EXCLUSIVE, so one line past the last is how a declaration
        // ending at EOF is spelled — but only at that line's start.
        assert!(definition_fits(
            span(position(2, 0), position(3, 0)),
            total,
            file
        ));
        assert!(!definition_fits(
            span(position(2, 0), position(3, 2)),
            total,
            file
        ));

        // Start inside but end past EOF: previously accepted, and advertised
        // verbatim beside a preview that did not contain it.
        assert!(!definition_fits(
            span(position(1, 0), position(9, 0)),
            total,
            file
        ));
        // Backwards ranges are incoherent on either axis.
        assert!(!definition_fits(
            span(position(2, 0), position(1, 0)),
            total,
            file
        ));
        assert!(!definition_fits(
            span(position(1, 3), position(1, 1)),
            total,
            file
        ));
        // Start past EOF stays refused.
        assert!(!definition_fits(
            span(position(9, 0), position(9, 4)),
            total,
            file
        ));

        // Columns are byte offsets into a REAL line: past its end is invalid...
        assert!(!definition_fits(
            span(position(0, 0), position(1, 9)),
            total,
            file
        ));
        assert!(!definition_fits(
            span(position(0, 7), position(1, 1)),
            total,
            file
        ));
        // ...and so is an offset that splits a character.
        assert!(!definition_fits(
            span(position(1, 0), position(1, 3)),
            total,
            file
        ));
        assert!(definition_fits(
            span(position(1, 0), position(1, 2)),
            total,
            file
        ));
    }

    #[test]
    fn clip_keeps_multibyte_lines_intact() {
        let lines = vec!["héllo — wörld", "ok"];
        let (text, kept, _) = clip(&lines, 1, usize::MAX);
        assert_eq!(text, "héllo — wörld");
        assert_eq!(kept, 1);
        // Never split inside a character: the kept text is exactly the line.
        assert!(text.is_char_boundary(text.len()));
    }

    #[test]
    fn context_window_is_padded_and_clamped_to_the_file() {
        let source = EntitySource::from_full_text(
            "src/lib.rs",
            // "line 1" is 6 bytes, so the span covers its final character.
            SourceRange {
                start: crate::code::substrate::Position { line: 1, col: 5 },
                end: crate::code::substrate::Position { line: 1, col: 6 },
            },
            &(0..5)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
            Scope::Context,
        );
        let rendered = render_source(&source, Scope::Context).expect("rendered");
        // The padding reaches past both ends, so the whole 5-line file is shown.
        assert_eq!(rendered.start_line, 1);
        assert_eq!(rendered.end_line, 5);
        assert_eq!(rendered.total_lines, 5);
        assert!(!rendered.truncated);
        assert_eq!(rendered.text, "line 0\nline 1\nline 2\nline 3\nline 4");
        // 0-based (1,5)..(1,6) becomes 1-based (2,6)..(2,7).
        assert_eq!(
            rendered.definition,
            Some(SourceSpan {
                start_line: 2,
                start_col: 6,
                end_line: 2,
                end_col: 7,
            })
        );
    }

    #[test]
    fn context_window_excludes_lines_beyond_the_padding() {
        let text = (0..60)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let source = EntitySource::from_full_text(
            "src/lib.rs",
            SourceRange {
                start: crate::code::substrate::Position { line: 30, col: 0 },
                end: crate::code::substrate::Position { line: 30, col: 4 },
            },
            &text,
            Scope::Context,
        );
        let rendered = render_source(&source, Scope::Context).expect("rendered");
        assert_eq!(rendered.start_line, 19);
        assert_eq!(rendered.end_line, 43);
        assert!(rendered.text.starts_with("line 18\n"));
        assert!(rendered.text.ends_with("\nline 42"));
    }
}
