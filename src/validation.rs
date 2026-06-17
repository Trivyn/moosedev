//! Lightweight, shape-graph-driven validation for recorded project knowledge.
//!
//! This is an intentional SHACL subset for M3: required literal presence
//! (`sh:minCount 1`) and literal datatype checks (`sh:datatype`). Other declared
//! constraints are counted as skipped so the report stays honest.

use oxigraph::model::Term;
use oxigraph::sparql::{QueryResults, SparqlEvaluator};

use crate::graph::AppState;
use crate::ontology::{ARCH_SHAPES_GRAPH_IRI, SE_SHAPES_GRAPH_IRI};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const SH_NODE_SHAPE: &str = "http://www.w3.org/ns/shacl#NodeShape";
const SH_TARGET_CLASS: &str = "http://www.w3.org/ns/shacl#targetClass";
const SH_PROPERTY: &str = "http://www.w3.org/ns/shacl#property";
const SH_PATH: &str = "http://www.w3.org/ns/shacl#path";
const SH_MIN_COUNT: &str = "http://www.w3.org/ns/shacl#minCount";
const SH_DATATYPE: &str = "http://www.w3.org/ns/shacl#datatype";
const SH_CLASS: &str = "http://www.w3.org/ns/shacl#class";
const SH_NODE: &str = "http://www.w3.org/ns/shacl#node";
const SH_IN: &str = "http://www.w3.org/ns/shacl#in";
const SH_MAX_COUNT: &str = "http://www.w3.org/ns/shacl#maxCount";
const SH_QUALIFIED_VALUE_SHAPE: &str = "http://www.w3.org/ns/shacl#qualifiedValueShape";
const SH_OR: &str = "http://www.w3.org/ns/shacl#or";

#[derive(Debug, Clone)]
struct PropertyConstraint {
    target_class: String,
    path: String,
    min_count: Option<u32>,
    datatype: Option<String>,
    skipped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViolationKind {
    MissingRequired,
    DatatypeMismatch,
}

#[derive(Debug, Clone)]
pub struct Violation {
    pub node: String,
    pub target_class: String,
    pub path: String,
    pub kind: ViolationKind,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct ValidationReport {
    pub violations: Vec<Violation>,
    pub shapes_checked: usize,
    pub skipped: usize,
}

impl ValidationReport {
    /// Whether the report has no validation violations.
    pub fn conforms(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Validate recorded project knowledge against the loaded architecture shapes.
pub fn validate_project(state: &AppState) -> anyhow::Result<ValidationReport> {
    let constraints = load_property_constraints(state)?;
    let shapes_checked = constraints.len();
    let mut skipped = count_node_level_skips(state)?;
    let mut violations = Vec::new();

    for constraint in constraints {
        skipped += constraint.skipped;
        if constraint.min_count == Some(1) {
            violations.extend(missing_required_violations(state, &constraint)?);
        } else if constraint.min_count.is_some() {
            skipped += 1;
        }
        if constraint.datatype.is_some() {
            violations.extend(datatype_violations(state, &constraint)?);
        }
    }

    Ok(ValidationReport {
        violations,
        shapes_checked,
        skipped,
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
        "\nShapes checked: {}\nUnsupported constraints skipped: {}\nViolations: {}",
        report.shapes_checked,
        report.skipped,
        report.violations.len()
    ));
    for violation in &report.violations {
        out.push_str(&format!(
            "\n\n- {:?}: {}\n  node: {}\n  target: {}\n  path: {}",
            violation.kind,
            violation.detail,
            violation.node,
            local_name(&violation.target_class),
            local_name(&violation.path)
        ));
    }
    out
}

/// Read every property shape from the loaded shape graphs and keep only the
/// fields this validator knows how to enforce.
fn load_property_constraints(state: &AppState) -> anyhow::Result<Vec<PropertyConstraint>> {
    let sparql = format!(
        r#"
SELECT ?target ?path ?minCount ?datatype ?class ?node ?in ?maxCount ?qualified
WHERE {{
  VALUES ?shapeGraph {{ <{SE_SHAPES_GRAPH_IRI}> <{ARCH_SHAPES_GRAPH_IRI}> }}
  GRAPH ?shapeGraph {{
    ?shape a <{SH_NODE_SHAPE}> ;
           <{SH_TARGET_CLASS}> ?target ;
           <{SH_PROPERTY}> ?property .
    ?property <{SH_PATH}> ?path .
    OPTIONAL {{ ?property <{SH_MIN_COUNT}> ?minCount }}
    OPTIONAL {{ ?property <{SH_DATATYPE}> ?datatype }}
    OPTIONAL {{ ?property <{SH_CLASS}> ?class }}
    OPTIONAL {{ ?property <{SH_NODE}> ?node }}
    OPTIONAL {{ ?property <{SH_IN}> ?in }}
    OPTIONAL {{ ?property <{SH_MAX_COUNT}> ?maxCount }}
    OPTIONAL {{ ?property <{SH_QUALIFIED_VALUE_SHAPE}> ?qualified }}
  }}
}}"#
    );

    let QueryResults::Solutions(solutions) = query(&state.store, &sparql)? else {
        anyhow::bail!("constraint query did not return solutions");
    };

    let mut constraints = Vec::new();
    for solution in solutions {
        let solution = solution?;
        let Some(target_class) = iri_value(solution.get("target")) else {
            continue;
        };
        let Some(path) = iri_value(solution.get("path")) else {
            continue;
        };
        let skipped = ["class", "node", "in", "maxCount", "qualified"]
            .iter()
            .filter(|name| solution.get(**name).is_some())
            .count();
        constraints.push(PropertyConstraint {
            target_class,
            path,
            min_count: integer_value(solution.get("minCount")),
            datatype: iri_value(solution.get("datatype")),
            skipped,
        });
    }
    Ok(constraints)
}

/// Count declared node-level constraints outside the M3 subset so validation
/// reports do not imply full SHACL coverage.
fn count_node_level_skips(state: &AppState) -> anyhow::Result<usize> {
    let sparql = format!(
        r#"
SELECT ?shape ?or
WHERE {{
  VALUES ?shapeGraph {{ <{SE_SHAPES_GRAPH_IRI}> <{ARCH_SHAPES_GRAPH_IRI}> }}
  GRAPH ?shapeGraph {{
    ?shape a <{SH_NODE_SHAPE}> ;
           <{SH_OR}> ?or .
  }}
}}"#
    );
    let QueryResults::Solutions(solutions) = query(&state.store, &sparql)? else {
        return Ok(0);
    };
    let mut count = 0;
    for solution in solutions {
        solution?;
        count += 1;
    }
    Ok(count)
}

/// Find instances of the target class hierarchy that do not have the required
/// predicate at all.
fn missing_required_violations(
    state: &AppState,
    constraint: &PropertyConstraint,
) -> anyhow::Result<Vec<Violation>> {
    // RDFS subclass traversal happens in the query because the store does not
    // perform inference; project instances and ontology classes live in
    // different named graphs, so validation queries use the union graph.
    let sparql = format!(
        r#"
SELECT DISTINCT ?node
WHERE {{
  ?node <{RDF_TYPE}>/<{RDFS_SUBCLASS_OF}>* <{}> .
  FILTER NOT EXISTS {{ ?node <{}> ?value }}
}}"#,
        constraint.target_class, constraint.path
    );
    let QueryResults::Solutions(solutions) = query(&state.store, &sparql)? else {
        return Ok(Vec::new());
    };

    let mut violations = Vec::new();
    for solution in solutions {
        let solution = solution?;
        if let Some(node) = iri_or_blank_value(solution.get("node")) {
            violations.push(Violation {
                node,
                target_class: constraint.target_class.clone(),
                path: constraint.path.clone(),
                kind: ViolationKind::MissingRequired,
                detail: format!("missing required {}", local_name(&constraint.path)),
            });
        }
    }
    Ok(violations)
}

/// Find values that exist for a constrained predicate but are not literals with
/// the shape-declared datatype.
fn datatype_violations(
    state: &AppState,
    constraint: &PropertyConstraint,
) -> anyhow::Result<Vec<Violation>> {
    let Some(datatype) = &constraint.datatype else {
        return Ok(Vec::new());
    };
    let sparql = format!(
        r#"
SELECT DISTINCT ?node ?value
WHERE {{
  ?node <{RDF_TYPE}>/<{RDFS_SUBCLASS_OF}>* <{}> .
  ?node <{}> ?value .
  FILTER (!isLiteral(?value) || datatype(?value) != <{}>)
}}"#,
        constraint.target_class, constraint.path, datatype
    );
    let QueryResults::Solutions(solutions) = query(&state.store, &sparql)? else {
        return Ok(Vec::new());
    };

    let mut violations = Vec::new();
    for solution in solutions {
        let solution = solution?;
        if let Some(node) = iri_or_blank_value(solution.get("node")) {
            violations.push(Violation {
                node,
                target_class: constraint.target_class.clone(),
                path: constraint.path.clone(),
                kind: ViolationKind::DatatypeMismatch,
                detail: format!(
                    "{} must have datatype {datatype}",
                    local_name(&constraint.path)
                ),
            });
        }
    }
    Ok(violations)
}

/// Run validator-owned SPARQL against the union graph; these queries are built
/// from constants and loaded IRIs, so parse failures indicate a template bug.
fn query<'a>(store: &'a oxigraph::store::Store, sparql: &str) -> anyhow::Result<QueryResults<'a>> {
    let mut prepared = SparqlEvaluator::new()
        .parse_query(sparql)
        .map_err(|e| anyhow::anyhow!("validation query parse failed: {e}\n{sparql}"))?;
    prepared.dataset_mut().set_default_graph_as_union();
    prepared
        .on_store(store)
        .execute()
        .map_err(|e| anyhow::anyhow!("validation query failed: {e}"))
}

/// Extract an IRI binding from a SPARQL solution.
fn iri_value(term: Option<&Term>) -> Option<String> {
    match term {
        Some(Term::NamedNode(node)) => Some(node.as_str().to_string()),
        _ => None,
    }
}

/// Extract a subject binding for violation reports.
fn iri_or_blank_value(term: Option<&Term>) -> Option<String> {
    match term {
        Some(Term::NamedNode(node)) => Some(node.as_str().to_string()),
        Some(Term::BlankNode(node)) => Some(node.as_str().to_string()),
        _ => None,
    }
}

/// Parse an integer-valued SHACL literal such as `sh:minCount`.
fn integer_value(term: Option<&Term>) -> Option<u32> {
    match term {
        Some(Term::Literal(literal)) => literal.value().parse().ok(),
        _ => None,
    }
}

/// Render the final IRI segment in reports.
fn local_name(iri: &str) -> &str {
    iri.rsplit(['/', '#']).next().unwrap_or(iri)
}
