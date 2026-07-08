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
    if ::scip::symbol::is_local_symbol(raw) {
        return None;
    }

    let mut symbol = parse_symbol(raw).ok()?;
    if let Some(package) = symbol.package.as_mut() {
        package.version.clear();
    }
    Some(format_symbol(symbol))
}

/// Human-readable descriptor path, e.g. "runtime::build_server", derived from
/// the parsed descriptor names joined with "::".
pub fn logical_path(raw: &str) -> Option<String> {
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

/// True when the symbol's last descriptor is a namespace, i.e. the symbol names
/// a module.
pub fn is_module_symbol(raw: &str) -> bool {
    parse_symbol(raw).ok().and_then(|symbol| {
        symbol
            .descriptors
            .last()
            .and_then(|descriptor| descriptor.suffix.enum_value().ok())
    }) == Some(descriptor::Suffix::Namespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FUNCTION: &str = "rust-analyzer cargo moosedev 0.6.3 runtime/build_server().";
    const NORMALIZED_FUNCTION: &str = "rust-analyzer cargo moosedev . runtime/build_server().";

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
}
