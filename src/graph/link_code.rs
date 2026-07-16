//! Link typed knowledge records to lazily minted CodeEntity nodes.

use std::collections::BTreeSet;

use anyhow::Context;
use oxigraph::model::NamedNode;

use crate::code::substrate::Position;

use super::capture::asserted_project_types;
use super::code_entities::{desired_name, ensure_entity, CodeTerms};
use super::components::load_components;
use super::lifecycle::relate;
use super::relations::EdgeDirection;
use super::state::AppState;

/// One code selector: a 1-based file position or a SCIP symbol (raw or normalized).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeSelector {
    Position { file: String, line: u32, col: u32 },
    Symbol(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkCodeOutcome {
    pub entity_iri: String,
    pub entity_name: String,
    pub created: bool,
    pub subject_iri: String,
    pub predicate_local: String,
    pub object_iri: String,
    pub substrate_stale: bool,
}

pub fn link_code(
    state: &AppState,
    record_iri: &str,
    predicate_local: &str,
    selector: &CodeSelector,
    agent: &str,
) -> anyhow::Result<LinkCodeOutcome> {
    let substrate = state.substrate().ok_or_else(|| {
        anyhow::anyhow!("code substrate unavailable; run `moosedev index` and restart the backend")
    })?;
    let entry = match selector {
        CodeSelector::Position { file, line, col } => {
            if *line == 0 || *col == 0 {
                anyhow::bail!("code positions are 1-based; line and col must be greater than 0");
            }
            let resolution = substrate.resolve(
                file,
                Position {
                    line: line - 1,
                    col: col - 1,
                },
            );
            let resolution = match resolution {
                Some(resolution) => resolution,
                None if !substrate.can_anchor(file) => anyhow::bail!(
                    "`{file}` is not in the code substrate (indexed: {}); cannot resolve a code entity here.",
                    substrate.describe_coverage()
                ),
                None => anyhow::bail!("no code entity at {file}:{line}:{col}"),
            };
            if resolution.is_local {
                anyhow::bail!(
                    "resolved symbol is a local; locals are not stable cross-file identities, select the definition of a named item instead"
                );
            }
            substrate
                .definition_for_symbol(&resolution.symbol)
                .ok_or_else(|| no_workspace_definition(&resolution.symbol))?
        }
        CodeSelector::Symbol(symbol) => substrate
            .definition_for_symbol(symbol)
            .ok_or_else(|| no_workspace_definition(symbol))?,
    };

    let terms = CodeTerms::resolve(state)?;
    let record_node =
        NamedNode::new(record_iri).with_context(|| format!("invalid record IRI {record_iri}"))?;
    let record_classes = asserted_project_types(state, &record_node);
    if record_classes.is_empty() {
        anyhow::bail!("unknown record IRI {record_iri}: no rdf:type in the project graph");
    }
    let (subject_is_record, legal) = orient_edge(
        state,
        &record_classes,
        &terms.code_entity_class,
        predicate_local,
    )?;

    let components = load_components(state)?;
    let entity = ensure_entity(state, &terms, &components, &entry, agent)?;
    let (subject_iri, object_iri) = if subject_is_record {
        let out = relate(state, record_iri, predicate_local, &entity.iri)?;
        (out.subject_iri, out.object_iri)
    } else {
        let out = relate(state, &entity.iri, predicate_local, record_iri)?;
        (out.subject_iri, out.object_iri)
    };

    Ok(LinkCodeOutcome {
        entity_iri: entity.iri,
        entity_name: desired_name(&entry),
        created: entity.created,
        subject_iri,
        predicate_local: legal,
        object_iri,
        substrate_stale: substrate.is_stale(),
    })
}

fn orient_edge(
    state: &AppState,
    record_classes: &[String],
    code_entity_class: &str,
    predicate_local: &str,
) -> anyhow::Result<(bool, String)> {
    let mut legal_predicates = Vec::new();
    for record_class in record_classes {
        legal_predicates.extend(state.catalogue.legal_predicates(
            &state.store,
            record_class,
            code_entity_class,
        ));
    }

    if legal_predicates.iter().any(|edge| {
        edge.predicate_local == predicate_local && edge.direction == EdgeDirection::Forward
    }) {
        return Ok((true, predicate_local.to_string()));
    }
    if legal_predicates.iter().any(|edge| {
        edge.predicate_local == predicate_local && edge.direction == EdgeDirection::Inverse
    }) {
        return Ok((false, predicate_local.to_string()));
    }

    let alternatives = legal_predicates
        .iter()
        .map(|edge| edge.predicate_local.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let suffix = if alternatives.is_empty() {
        "no legal predicates are declared for these classes".to_string()
    } else {
        format!("legal predicates: {}", alternatives.join(", "))
    };
    anyhow::bail!(
        "predicate {predicate_local:?} is not legal between the record and CodeEntity; {suffix}"
    );
}

fn no_workspace_definition(symbol: &str) -> anyhow::Error {
    anyhow::anyhow!("symbol {symbol:?} has no workspace definition")
}
