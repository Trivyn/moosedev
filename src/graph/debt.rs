//! Why-coverage comprehension-debt metric (v2.1, role-independent).
//!
//! Per `SystemComponent`, the fraction of its public code surface that carries
//! at least one linked rationale record. The "surface" is the substrate's
//! public, non-module, non-test definitions ([`is_debt_surface`]) mapped to a
//! component by path ([`best_component_for_path`]); "documented" reuses the same
//! [`direct_records_for_entity`] oracle the dossier and hover use, so the metric
//! counts exactly what a human sees on hover. Proposed links carry no real edge,
//! so they never count — the number only moves on ratification.

use std::collections::BTreeMap;

use crate::code::substrate::DefinitionEntry;

use super::code_entities::{entities_by_symbol, is_test_path, CodeTerms};
use super::components::{best_component_for_path, load_components};
use super::dossier::direct_records_for_entity;
use super::state::AppState;

/// Why-coverage for one `SystemComponent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentCoverage {
    pub iri: Option<String>,
    pub name: String,
    /// Public-surface entities with ≥1 linked rationale record.
    pub numerator: usize,
    /// Total public-surface entities owned by this component.
    pub denominator: usize,
    /// Names of the undocumented public-surface entities (auditable drill-down).
    pub undocumented: Vec<String>,
}

impl ComponentCoverage {
    /// Documented fraction, or `None` when the component owns no public surface
    /// (0/0 is "nothing to document", not "fully documented").
    pub fn ratio(&self) -> Option<f64> {
        (self.denominator > 0).then(|| self.numerator as f64 / self.denominator as f64)
    }
}

/// Per-component why-coverage plus the count of public-surface definitions whose
/// path maps to no component (reported, never charged to a component).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhyCoverage {
    pub components: Vec<ComponentCoverage>,
    pub unmapped: usize,
}

/// A public, non-module, non-test definition — the debt surface a human counts
/// as "the public API of this component". Shared verbatim with the LSP hotspot
/// lens so the metric and the in-editor flag never disagree.
pub(crate) fn is_debt_surface(entry: &DefinitionEntry) -> bool {
    entry.is_public && !entry.is_module && !is_test_path(&entry.file)
}

fn component_key(iri: Option<&String>, name: &str) -> String {
    iri.cloned().unwrap_or_else(|| format!("name:{name}"))
}

/// Compute per-component why-coverage over the loaded substrate + project graph.
pub fn compute_why_coverage(state: &AppState) -> anyhow::Result<WhyCoverage> {
    let components = load_components(state)?;

    // Seed every component (including zero-surface ones) so the report is total.
    let mut acc: BTreeMap<String, ComponentCoverage> = components
        .iter()
        .map(|c| {
            (
                component_key(c.iri.as_ref(), &c.name),
                ComponentCoverage {
                    iri: c.iri.clone(),
                    name: c.name.clone(),
                    numerator: 0,
                    denominator: 0,
                    undocumented: Vec::new(),
                },
            )
        })
        .collect();

    let Some(substrate) = state.substrate() else {
        // No index loaded — every component is trivially 0/0.
        let mut components: Vec<_> = acc.into_values().collect();
        components.sort_by(|a, b| a.name.cmp(&b.name));
        return Ok(WhyCoverage {
            components,
            unmapped: 0,
        });
    };

    let terms = CodeTerms::resolve(state)?;
    let entities = entities_by_symbol(state, &terms)?;
    let mut unmapped = 0usize;

    for def in substrate.definitions() {
        if !is_debt_surface(&def) {
            continue;
        }
        let Some(component) = best_component_for_path(&def.file, &components) else {
            unmapped += 1;
            continue;
        };
        let key = component_key(component.iri.as_ref(), &component.name);
        let entry = acc.get_mut(&key).expect("every component was seeded above");
        entry.denominator += 1;

        let documented = match entities.get(&def.normalized_symbol) {
            Some(iri) => !direct_records_for_entity(state, iri)?.is_empty(),
            None => false,
        };
        if documented {
            entry.numerator += 1;
        } else {
            entry
                .undocumented
                .push(def.display_name.clone().unwrap_or(def.normalized_symbol));
        }
    }

    let mut components: Vec<ComponentCoverage> = acc.into_values().collect();
    for c in &mut components {
        c.undocumented.sort();
        c.undocumented.dedup();
    }
    components.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(WhyCoverage {
        components,
        unmapped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file: &str, is_public: bool, is_module: bool) -> DefinitionEntry {
        DefinitionEntry {
            producer: "rust-analyzer".to_string(),
            symbol: "rust-analyzer cargo p 0.1.0 foo/x().".to_string(),
            normalized_symbol: "rust-analyzer cargo p . foo/x().".to_string(),
            display_name: Some("x".to_string()),
            kind: Some("Function".to_string()),
            signature: Some("pub fn x()".to_string()),
            file: file.to_string(),
            is_module,
            is_public,
        }
    }

    #[test]
    fn debt_surface_is_public_nonmodule_nontest() {
        assert!(is_debt_surface(&entry("src/foo/a.rs", true, false)));
        assert!(
            !is_debt_surface(&entry("src/foo/a.rs", false, false)),
            "private excluded"
        );
        assert!(
            !is_debt_surface(&entry("src/foo/a.rs", true, true)),
            "module excluded"
        );
        assert!(
            !is_debt_surface(&entry("tests/foo.rs", true, false)),
            "tests/ prefix excluded"
        );
        assert!(
            !is_debt_surface(&entry("src/foo/tests/a.rs", true, false)),
            "test path segment excluded"
        );
    }

    #[test]
    fn ratio_is_none_for_zero_surface() {
        let c = ComponentCoverage {
            iri: None,
            name: "empty".to_string(),
            numerator: 0,
            denominator: 0,
            undocumented: Vec::new(),
        };
        assert_eq!(c.ratio(), None);
    }
}
