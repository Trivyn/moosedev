//! SHACL validation for recorded project knowledge.
//!
//! Validation is delegated to MOOSE's SNARL-backed SHACL Core validator. Warning
//! severity results are used for non-blocking graph-density advisories; violation
//! severity results are the only findings that affect conformance.

use std::collections::HashSet;

use moose::shacl::{ShaclReport, ShaclSeverity, ShaclViolation};
use oxigraph::model::{GraphNameRef, NamedNodeRef, TermRef};

use crate::graph::{AppState, PROJECT_KG_GRAPH_IRI};
use crate::ontology::{
    ARCH_DOMAIN_GRAPH_IRI, ARCH_SHAPES_GRAPH_IRI, SE_DOMAIN_GRAPH_IRI, SE_SHAPES_GRAPH_IRI,
};

const SH_NODE_SHAPE: &str = "http://www.w3.org/ns/shacl#NodeShape";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViolationKind {
    MissingRequired,
    DatatypeMismatch,
    Other(String),
}

#[derive(Debug, Clone)]
pub struct Violation {
    pub node: String,
    pub source_shape: String,
    pub path: String,
    pub kind: ViolationKind,
    pub detail: String,
}

/// A non-blocking "SHOULD carry a link" finding, sourced from SHACL Warning
/// results declared in the shape graph.
#[derive(Debug, Clone)]
pub struct Advisory {
    pub node: String,
    pub target_class: String,
    pub missing_predicate: String,
}

#[derive(Debug, Clone)]
pub struct ValidationReport {
    pub violations: Vec<Violation>,
    /// Non-blocking under-linked findings. These do NOT affect `conforms()`;
    /// they nudge the agent to densify the graph via `suggest_links` / `relate`.
    pub advisories: Vec<Advisory>,
    pub shapes_checked: usize,
}

impl ValidationReport {
    /// Whether the report has no validation violations. Advisories are non-blocking
    /// and deliberately excluded.
    pub fn conforms(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Run SNARL over the project graph plus the loaded ontology T-boxes.
pub(crate) fn run_project_shacl(state: &AppState) -> anyhow::Result<ShaclReport> {
    moose::shacl::validate_graphs(
        &state.store,
        &[PROJECT_KG_GRAPH_IRI.to_string()],
        &[
            SE_DOMAIN_GRAPH_IRI.to_string(),
            ARCH_DOMAIN_GRAPH_IRI.to_string(),
        ],
        &[
            SE_SHAPES_GRAPH_IRI.to_string(),
            ARCH_SHAPES_GRAPH_IRI.to_string(),
        ],
    )
    .map_err(|e| anyhow::anyhow!("SHACL validation failed: {e}"))
}

/// Validate recorded project knowledge against the loaded architecture shapes.
pub fn validate_project(state: &AppState) -> anyhow::Result<ValidationReport> {
    let report = run_project_shacl(state)?;
    let violations = report
        .violations
        .iter()
        .filter(|v| v.severity == ShaclSeverity::Violation)
        .map(map_violation)
        .collect();
    let advisories = crate::graph::under_linked_from_report(state, &report, usize::MAX)
        .into_iter()
        .map(|u| Advisory {
            node: u.iri,
            target_class: u.class_local,
            missing_predicate: u.missing_predicate,
        })
        .collect();

    Ok(ValidationReport {
        violations,
        advisories,
        shapes_checked: count_target_shapes(state)?,
    })
}

/// Render a validation report for an agent or human reading MCP output.
pub fn format_report(report: &ValidationReport) -> String {
    let mut out = if report.conforms() {
        "Conforms: true".to_string()
    } else {
        "Conforms: false".to_string()
    };
    out.push_str(&format!(
        "\nShapes checked: {}\nViolations: {}",
        report.shapes_checked,
        report.violations.len()
    ));
    for violation in &report.violations {
        out.push_str(&format!(
            "\n\n- {:?}: {}\n  node: {}\n  shape: {}\n  path: {}",
            violation.kind,
            violation.detail,
            violation.node,
            local_name(&violation.source_shape),
            local_name(&violation.path)
        ));
    }
    if !report.advisories.is_empty() {
        // Show the count plus a bounded sample; the full set is the `suggest_links`
        // scan's job, not validate's.
        const ADVISORY_SAMPLE: usize = 10;
        out.push_str(&format!(
            "\n\nAdvisories (SHOULD, non-blocking): {} — run `suggest_links` for candidates",
            report.advisories.len()
        ));
        for advisory in report.advisories.iter().take(ADVISORY_SAMPLE) {
            out.push_str(&format!(
                "\n  - {} ({}) should carry {}",
                advisory.node, advisory.target_class, advisory.missing_predicate
            ));
        }
        let remaining = report.advisories.len().saturating_sub(ADVISORY_SAMPLE);
        if remaining > 0 {
            out.push_str(&format!("\n  … and {remaining} more"));
        }
    }
    out
}

fn map_violation(v: &ShaclViolation) -> Violation {
    Violation {
        node: v.focus_node.clone(),
        source_shape: v.source_shape.clone(),
        path: v.result_path.clone().unwrap_or_default(),
        kind: classify(&v.source_constraint_component),
        detail: v.message.clone().unwrap_or_else(|| {
            let mut detail = local_name(&v.source_constraint_component).to_string();
            if let Some(value) = &v.value {
                detail.push_str(&format!(" (value: {value})"));
            }
            detail
        }),
    }
}

fn classify(component: &str) -> ViolationKind {
    let lowered = component.to_ascii_lowercase();
    if lowered.contains("mincount") {
        ViolationKind::MissingRequired
    } else if lowered.contains("datatype") {
        ViolationKind::DatatypeMismatch
    } else {
        // Match case-insensitively, but preserve the original casing in the report
        // (e.g. `Other("OrConstraintComponent")`, not lowercased).
        ViolationKind::Other(local_name(component).to_string())
    }
}

fn count_target_shapes(state: &AppState) -> anyhow::Result<usize> {
    let rdf_type = NamedNodeRef::new(moose::RDF_TYPE)?;
    let node_shape = NamedNodeRef::new(SH_NODE_SHAPE)?;
    let mut shapes = HashSet::new();
    for graph_iri in [SE_SHAPES_GRAPH_IRI, ARCH_SHAPES_GRAPH_IRI] {
        let graph = NamedNodeRef::new(graph_iri)?;
        for quad in state.store.quads_for_pattern(
            None,
            Some(rdf_type),
            Some(TermRef::NamedNode(node_shape)),
            Some(GraphNameRef::NamedNode(graph)),
        ) {
            let quad = quad.map_err(|e| anyhow::anyhow!("count SHACL shapes: {e}"))?;
            shapes.insert(quad.subject.to_string());
        }
    }
    Ok(shapes.len())
}

/// Render the final IRI segment in reports.
fn local_name(iri: &str) -> &str {
    iri.rsplit(['/', '#']).next().unwrap_or(iri)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxigraph::model::{BlankNode, GraphName, Literal, NamedNode, Quad};

    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    const SH_TARGET_CLASS: &str = "http://www.w3.org/ns/shacl#targetClass";
    const SH_PROPERTY: &str = "http://www.w3.org/ns/shacl#property";
    const SH_PATH: &str = "http://www.w3.org/ns/shacl#path";
    const SH_MIN_COUNT: &str = "http://www.w3.org/ns/shacl#minCount";
    const SH_SEVERITY: &str = "http://www.w3.org/ns/shacl#severity";
    const SH_WARNING: &str = "http://www.w3.org/ns/shacl#Warning";
    const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
    const PERSON: &str = "http://example.org/Person";
    const PERSON_SHAPE: &str = "http://example.org/PersonShape";
    const NAME: &str = "http://example.org/name";
    const ALICE: &str = "http://example.org/alice";

    fn nn(iri: &str) -> NamedNode {
        NamedNode::new(iri).unwrap()
    }

    fn triple(
        subject: impl Into<oxigraph::model::NamedOrBlankNode>,
        predicate: &str,
        object: impl Into<oxigraph::model::Term>,
    ) -> Quad {
        Quad::new(subject, nn(predicate), object, GraphName::DefaultGraph)
    }

    #[test]
    fn snarl_min_count_warning_carries_warning_severity_and_path() {
        let property_shape = BlankNode::new("name-warning").unwrap();
        let shapes = vec![
            triple(nn(PERSON_SHAPE), RDF_TYPE, nn(SH_NODE_SHAPE)),
            triple(nn(PERSON_SHAPE), SH_TARGET_CLASS, nn(PERSON)),
            triple(nn(PERSON_SHAPE), SH_PROPERTY, property_shape.clone()),
            triple(property_shape.clone(), SH_PATH, nn(NAME)),
            triple(
                property_shape.clone(),
                SH_MIN_COUNT,
                Literal::new_typed_literal("1", nn(XSD_INTEGER)),
            ),
            triple(property_shape, SH_SEVERITY, nn(SH_WARNING)),
        ];
        let data = vec![triple(nn(ALICE), RDF_TYPE, nn(PERSON))];

        let report = moose::shacl::validate_graph(&data, &shapes).expect("validation runs");
        assert!(
            !report.conforms,
            "SNARL's raw report treats any validation result as non-conforming; MOOSEDev filters by severity: {report:?}"
        );
        assert!(
            report.violations.iter().any(|v| {
                v.severity == ShaclSeverity::Warning
                    && v.focus_node == ALICE
                    && v.result_path.as_deref() == Some(NAME)
            }),
            "expected a Warning result on {ALICE} for {NAME}; got {:?}",
            report.violations
        );
    }

    #[test]
    fn format_report_caps_advisory_listing_and_conforms() {
        let advisories: Vec<Advisory> = (0..15)
            .map(|i| Advisory {
                node: format!("urn:node:{i}"),
                target_class: "ArchitecturalDecision".to_string(),
                missing_predicate: "isMotivatedBy".to_string(),
            })
            .collect();
        let report = ValidationReport {
            violations: Vec::new(),
            advisories,
            shapes_checked: 0,
        };
        // Advisories are non-blocking: an all-advisory report still conforms.
        assert!(report.conforms());
        let out = format_report(&report);
        assert!(out.contains("Advisories (SHOULD, non-blocking): 15"));
        assert!(!out.contains("Unsupported constraints skipped"));
        assert!(out.contains("… and 5 more"));
        assert!(out.contains("urn:node:0")); // first sample shown
        assert!(!out.contains("urn:node:14")); // beyond the sample, elided
    }
}
