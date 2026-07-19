//! SCIP symbol grammar helpers for stable code-entity identity.
//!
//! These helpers use the `scip` crate parser/formatter directly. SCIP descriptor
//! names may be backtick-escaped and contain spaces, so callers must not split
//! raw symbol strings by hand.

use scip::symbol::{format_symbol, parse_symbol};
use scip::types::descriptor;

/// Normalized form of a SCIP symbol: the package version descriptor is elided
/// (Constraint 00b3986e) so entity identity survives crate-version bumps.
/// Returns None for local symbols and for symbols the SCIP grammar cannot
/// parse (report, never guess).
pub fn normalize_symbol(raw: &str) -> Option<String> {
    // Tree-sitter identities are already versionless and self-describing, so
    // Constraint 00b3986e is satisfied by preserving them verbatim.
    if raw.starts_with("ts:") {
        return Some(raw.to_string());
    }
    if ::scip::symbol::is_local_symbol(raw) {
        return None;
    }

    // Producer-idiom symbols (e.g. scip-python module markers) canonicalize
    // here — the shared identity boundary — so ingest, minting, and
    // caller-provided raw symbols all converge on one identity.
    let canonical = super::lang::canonical_symbol(raw);
    let raw = canonical.as_deref().unwrap_or(raw);

    let mut symbol = parse_symbol(raw).ok()?;
    if let Some(package) = symbol.package.as_mut() {
        package.version.clear();
    }
    Some(format_symbol(symbol))
}

/// Human-readable descriptor path, e.g. "runtime::build_server", derived from
/// the parsed descriptor names joined with "::".
pub fn logical_path(raw: &str) -> Option<String> {
    if raw.starts_with("ts:") {
        return syntactic_parts(raw).map(|(_, path)| path.to_string());
    }
    let symbol = parse_symbol(raw).ok()?;
    if symbol.descriptors.is_empty() {
        return None;
    }

    Some(
        symbol
            .descriptors
            .iter()
            .map(|descriptor| descriptor.name.as_str())
            .collect::<Vec<_>>()
            .join("::"),
    )
}

/// The final descriptor name, suitable as a display label when a producer
/// leaves `SymbolInformation.display_name` empty.
pub fn last_descriptor_name(raw: &str) -> Option<String> {
    if raw.starts_with("ts:") {
        return syntactic_parts(raw)
            .and_then(|(_, path)| path.rsplit("::").next().map(str::to_string));
    }
    parse_symbol(raw)
        .ok()?
        .descriptors
        .last()
        .map(|descriptor| descriptor.name.clone())
}

/// True for a parsed, non-module symbol whose ancestors are all namespaces.
pub(crate) fn is_top_level_declaration(raw: &str) -> bool {
    let Ok(symbol) = parse_symbol(raw) else {
        return false;
    };
    let Some((last, ancestors)) = symbol.descriptors.split_last() else {
        return false;
    };
    let suffix = |descriptor: &scip::types::Descriptor| {
        descriptor.suffix.enum_value().ok() == Some(descriptor::Suffix::Namespace)
    };
    ancestors.iter().all(suffix) && !suffix(last)
}

/// True when the symbol's last descriptor is a namespace, i.e. the symbol names
/// a module.
pub fn is_module_symbol(raw: &str) -> bool {
    if raw.starts_with("ts:") {
        return syntactic_parts(raw).is_some_and(|(kind, _)| kind == "mod");
    }
    parse_symbol(raw).ok().and_then(|symbol| {
        symbol
            .descriptors
            .last()
            .and_then(|descriptor| descriptor.suffix.enum_value().ok())
    }) == Some(descriptor::Suffix::Namespace)
}

/// Parse the kind and qualified-name fields of a self-describing syntactic
/// identity. `splitn` is load-bearing because qualified names contain `::`.
fn syntactic_parts(raw: &str) -> Option<(&str, &str)> {
    let mut parts = raw.splitn(5, ':');
    (parts.next()? == "ts").then_some(())?;
    let _language = parts.next()?;
    let _path = parts.next()?;
    let kind = parts.next()?;
    let qualified_name = parts.next()?;
    (!kind.is_empty() && !qualified_name.is_empty()).then_some((kind, qualified_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FUNCTION: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";
    const NORMALIZED_FUNCTION: &str = "rust-analyzer cargo moosedev . runtime/build_server().";
    const TS_MODULE: &str = "scip-typescript npm moosedev-ui 0.6.3 src/pages/`RecordPage.tsx`/";
    const TS_FUNCTION: &str =
        "scip-typescript npm moosedev-ui 0.6.3 src/pages/`RecordPage.tsx`/RecordPage().";
    const TS_INTERFACE: &str = "scip-typescript npm vis-fixture 1.2.3 src/`vis.ts`/ExportedIface#";
    const TS_PROPERTY: &str = "scip-typescript npm vis-fixture 1.2.3 src/`vis.ts`/ExportedIface#a.";
    const TS_METHOD: &str =
        "scip-typescript npm vis-fixture 1.2.3 src/`vis.ts`/ExportedClass#method().";
    const TS_PARAMETER: &str =
        "scip-typescript npm vis-fixture 1.2.3 src/`vis.ts`/exportedFn().(x)";

    #[test]
    fn normalizes_package_version() {
        assert_eq!(
            normalize_symbol(FUNCTION).as_deref(),
            Some(NORMALIZED_FUNCTION)
        );
    }

    #[test]
    fn normalization_is_idempotent() {
        assert_eq!(
            normalize_symbol(NORMALIZED_FUNCTION).as_deref(),
            Some(NORMALIZED_FUNCTION)
        );
    }

    #[test]
    fn local_symbols_do_not_normalize() {
        assert_eq!(normalize_symbol("local 1"), None);
    }

    #[test]
    fn package_absent_global_symbols_still_normalize() {
        let raw = "rust-analyzer . . . runtime/build_server().";

        assert_eq!(normalize_symbol(raw).as_deref(), Some(raw));
    }

    #[test]
    fn escaped_descriptor_names_round_trip() {
        let raw = "rust-analyzer cargo x 1.0 `weird name`/f().";

        assert_eq!(
            normalize_symbol(raw).as_deref(),
            Some("rust-analyzer cargo x . `weird name`/f().")
        );
    }

    #[test]
    fn detects_module_symbols() {
        assert!(is_module_symbol(
            "rust-analyzer cargo moosedev 0.6.3 runtime/"
        ));
        assert!(!is_module_symbol(FUNCTION));
    }

    #[test]
    fn logical_path_uses_descriptor_names() {
        assert_eq!(
            logical_path(FUNCTION).as_deref(),
            Some("runtime::build_server")
        );
    }

    #[test]
    fn typescript_symbols_normalize_idempotently_and_preserve_grammar() {
        for raw in [
            TS_MODULE,
            TS_FUNCTION,
            TS_INTERFACE,
            TS_PROPERTY,
            TS_METHOD,
            TS_PARAMETER,
        ] {
            let normalized = normalize_symbol(raw).unwrap();
            assert!(normalized.contains(" . "), "{normalized}");
            assert!(!normalized.contains(" 0.6.3 "), "{normalized}");
            assert!(!normalized.contains(" 1.2.3 "), "{normalized}");
            assert_eq!(
                normalize_symbol(&normalized).as_deref(),
                Some(normalized.as_str())
            );
        }
        assert!(is_module_symbol(TS_MODULE));
        assert!(!is_module_symbol(TS_FUNCTION));
        assert_eq!(
            logical_path(TS_FUNCTION).as_deref(),
            Some("src::pages::RecordPage.tsx::RecordPage")
        );
    }

    #[test]
    fn python_symbols_normalize_idempotently_and_preserve_grammar() {
        const PY_MODULE: &str = "scip-python python snapshot-util 0.1 class_nohint/__init__:";
        const PY_CLASS: &str = "scip-python python snapshot-util 0.1 class_nohint/Example#";
        const PY_METHOD: &str =
            "scip-python python snapshot-util 0.1 class_nohint/Example#__init__().";
        const PY_ATTR: &str = "scip-python python snapshot-util 0.1 class_nohint/Example#x.";
        const PY_PARAM: &str =
            "scip-python python snapshot-util 0.1 class_nohint/Example#__init__().(self)";

        for raw in [PY_MODULE, PY_CLASS, PY_METHOD, PY_ATTR, PY_PARAM] {
            let normalized = normalize_symbol(raw).unwrap();
            assert!(normalized.contains(" . "), "{normalized}");
            assert!(!normalized.contains(" 0.1 "), "{normalized}");
            assert_eq!(
                normalize_symbol(&normalized).as_deref(),
                Some(normalized.as_str())
            );
        }
        assert_eq!(
            logical_path(PY_METHOD).as_deref(),
            Some("class_nohint::Example::__init__")
        );
        assert_eq!(last_descriptor_name(PY_CLASS).as_deref(), Some("Example"));
        // The raw `pkg/__init__:` marker is a Meta descriptor, not a
        // namespace, so grammar-level classification stays honest here;
        // the identity boundary (normalize_symbol → lang::canonical_symbol)
        // rewrites markers to namespace module symbols before anything
        // classifies or compares them.
        assert!(!is_module_symbol(PY_MODULE));
        let normalized_module = normalize_symbol(PY_MODULE).unwrap();
        assert_eq!(
            normalized_module,
            "scip-python python snapshot-util . class_nohint/"
        );
        assert!(is_module_symbol(&normalized_module));
    }

    #[test]
    fn syntactic_identities_bridge_symbol_helpers() {
        let method = "ts:rust:tests/fixtures/ts_fallback.rs:fn:<Widget as Render>::render";
        let module = "ts:rust:tests/fixtures/ts_fallback.rs:mod:outer::inner";

        assert_eq!(normalize_symbol(method).as_deref(), Some(method));
        assert_eq!(
            logical_path(method).as_deref(),
            Some("<Widget as Render>::render")
        );
        assert_eq!(last_descriptor_name(method).as_deref(), Some("render"));
        assert!(!is_module_symbol(method));
        assert!(is_module_symbol(module));
    }
}
