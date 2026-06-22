//! PROV-O edit provenance.
//!
//! Records *who/what asserted each instance and when* into a companion
//! provenance graph (kept separate so the domain graph stays clean). This is
//! the write-side mirror of the query reasoning trace — together they make both
//! "why is this in the graph" and "why did the query conclude this" auditable
//! (invariant #6). It is also the MOOSEDev-side prototype of MOOSE's deferred
//! general `ProvenanceWriter` hook (core-moose-asks Ask 2, scope A).

use chrono::{DateTime, Utc};
use moose::{RDFS_LABEL, RDF_TYPE};
use oxigraph::model::{
    GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, NamedOrBlankNode, Quad, Term, Triple,
};
use oxigraph::store::Store;

/// Companion named graph holding PROV-O edit provenance.
pub const PROVENANCE_GRAPH_IRI: &str = "https://moosedev.dev/kg/provenance";

const PROV_ACTIVITY: &str = "http://www.w3.org/ns/prov#Activity";
const PROV_SOFTWARE_AGENT: &str = "http://www.w3.org/ns/prov#SoftwareAgent";
const PROV_WAS_GENERATED_BY: &str = "http://www.w3.org/ns/prov#wasGeneratedBy";
const PROV_WAS_ATTRIBUTED_TO: &str = "http://www.w3.org/ns/prov#wasAttributedTo";
const PROV_WAS_ASSOCIATED_WITH: &str = "http://www.w3.org/ns/prov#wasAssociatedWith";
const PROV_GENERATED_AT_TIME: &str = "http://www.w3.org/ns/prov#generatedAtTime";
const PROV_ENDED_AT_TIME: &str = "http://www.w3.org/ns/prov#endedAtTime";
const XSD_DATETIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";

fn nn(iri: &str) -> anyhow::Result<NamedNode> {
    NamedNode::new(iri).map_err(|e| anyhow::anyhow!("invalid IRI {iri:?}: {e}"))
}

/// Deterministic agent IRI keyed by name, so repeat edits by the same agent
/// share one `prov:Agent` node.
fn agent_iri(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("https://moosedev.dev/kg/agent/{slug}")
}

/// Record edit provenance for `entity_iri`, asserted by `agent_name`, now.
pub fn record_provenance(store: &Store, entity_iri: &str, agent_name: &str) -> anyhow::Result<()> {
    record_provenance_at(store, entity_iri, agent_name, Utc::now())
}

/// Record edit provenance for `entity_iri`, asserted by `agent_name`, at `when`.
pub fn record_provenance_at(
    store: &Store,
    entity_iri: &str,
    agent_name: &str,
    when: DateTime<Utc>,
) -> anyhow::Result<()> {
    // Every IRI but the caller-supplied `entity_iri` is a compile-time constant
    // or freshly minted here, so it's known-valid — `new_unchecked` skips the
    // per-write IRI re-parse and keeps the quad list readable.
    let graph = GraphName::NamedNode(NamedNode::new_unchecked(PROVENANCE_GRAPH_IRI));
    let entity = nn(entity_iri)?;
    let activity = NamedNode::new_unchecked(format!(
        "https://moosedev.dev/kg/Activity/{}",
        uuid::Uuid::new_v4()
    ));
    let agent = NamedNode::new_unchecked(agent_iri(agent_name));
    let ts = Literal::new_typed_literal(when.to_rfc3339(), NamedNode::new_unchecked(XSD_DATETIME));
    let rdf_type = NamedNode::new_unchecked(RDF_TYPE);

    let quads = [
        // The assertion activity.
        Quad::new(
            activity.clone(),
            rdf_type.clone(),
            NamedNode::new_unchecked(PROV_ACTIVITY),
            graph.clone(),
        ),
        Quad::new(
            activity.clone(),
            NamedNode::new_unchecked(PROV_WAS_ASSOCIATED_WITH),
            agent.clone(),
            graph.clone(),
        ),
        Quad::new(
            activity.clone(),
            NamedNode::new_unchecked(PROV_ENDED_AT_TIME),
            ts.clone(),
            graph.clone(),
        ),
        // The agent.
        Quad::new(
            agent.clone(),
            rdf_type,
            NamedNode::new_unchecked(PROV_SOFTWARE_AGENT),
            graph.clone(),
        ),
        Quad::new(
            agent.clone(),
            NamedNode::new_unchecked(RDFS_LABEL),
            Literal::new_simple_literal(agent_name),
            graph.clone(),
        ),
        // The entity's links back to the activity + agent.
        Quad::new(
            entity.clone(),
            NamedNode::new_unchecked(PROV_WAS_GENERATED_BY),
            activity,
            graph.clone(),
        ),
        Quad::new(
            entity.clone(),
            NamedNode::new_unchecked(PROV_WAS_ATTRIBUTED_TO),
            agent,
            graph.clone(),
        ),
        Quad::new(
            entity,
            NamedNode::new_unchecked(PROV_GENERATED_AT_TIME),
            ts,
            graph,
        ),
    ];

    let mut txn = store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("provenance transaction: {e}"))?;
    txn.extend(quads.iter().map(Quad::as_ref));
    txn.commit()
        .map_err(|e| anyhow::anyhow!("provenance commit: {e}"))?;
    Ok(())
}

/// Edit provenance recorded for a knowledge item.
pub struct Provenance {
    pub agent: String,
    pub time: String,
    pub activity: String,
}

/// Read the edit provenance recorded for `entity_iri`, if any.
pub fn read_provenance(store: &Store, entity_iri: &str) -> anyhow::Result<Option<Provenance>> {
    let entity = nn(entity_iri)?;
    let graph = NamedNode::new_unchecked(PROVENANCE_GRAPH_IRI);
    let g = GraphNameRef::NamedNode(graph.as_ref());

    let mut agent_node: Option<NamedNode> = None;
    let mut time = String::new();
    let mut activity = String::new();
    for q in store
        .quads_for_pattern(Some(entity.as_ref().into()), None, None, Some(g))
        .flatten()
    {
        let p = q.predicate.as_str();
        if p == PROV_WAS_ATTRIBUTED_TO {
            if let Term::NamedNode(n) = q.object {
                agent_node = Some(n);
            }
        } else if p == PROV_GENERATED_AT_TIME {
            if let Term::Literal(lit) = &q.object {
                time = lit.value().to_string();
            }
        } else if p == PROV_WAS_GENERATED_BY {
            if let Term::NamedNode(n) = &q.object {
                activity = n.as_str().to_string();
            }
        }
    }

    let Some(agent_node) = agent_node else {
        return Ok(None);
    };

    // Resolve the agent's label.
    let mut agent = agent_node.as_str().to_string();
    for q in store
        .quads_for_pattern(
            Some(agent_node.as_ref().into()),
            Some(NamedNodeRef::new_unchecked(RDFS_LABEL)),
            None,
            Some(g),
        )
        .flatten()
    {
        if let Term::Literal(lit) = &q.object {
            agent = lit.value().to_string();
            break;
        }
    }

    Ok(Some(Provenance {
        agent,
        time,
        activity,
    }))
}

/// Stable name of the GROWL reasoner agent. Reasoner-materialized triples are attributed
/// to it, and [`clear_reasoner_inferences`] finds prior inferences by it.
pub const REASONER_AGENT: &str = "growl-owl2rl";

/// `rdf:reifies` (RDF 1.2) — links a reifier to the triple term it is *about*.
const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";

/// Build the PROV-O + RDF 1.2 reification quads that attribute the inferred `delta`
/// (triples destined for the data graph) to a fresh reasoner-inference activity, for
/// insertion into the provenance graph. Performs **no** writes.
///
/// Each inferred triple `(s,p,o)` gets a reifier `R` with `R rdf:reifies «s p o»` and
/// `R prov:wasGeneratedBy <activity>`; the activity is `prov:wasAssociatedWith` the
/// reasoner agent. The reified triple term sits in OBJECT position — the only place RDF
/// 1.2 admits it — so every quad is writable through the ordinary Transaction API (no
/// SPARQL UPDATE, which this codebase deliberately rejects).
pub fn reasoner_inference_quads(delta: &[Quad], when: DateTime<Utc>) -> Vec<Quad> {
    let graph = GraphName::NamedNode(NamedNode::new_unchecked(PROVENANCE_GRAPH_IRI));
    let activity = NamedNode::new_unchecked(format!(
        "https://moosedev.dev/kg/Activity/{}",
        uuid::Uuid::new_v4()
    ));
    let agent = NamedNode::new_unchecked(agent_iri(REASONER_AGENT));
    let ts = Literal::new_typed_literal(when.to_rfc3339(), NamedNode::new_unchecked(XSD_DATETIME));
    let reifies = NamedNode::new_unchecked(RDF_REIFIES);
    let was_generated_by = NamedNode::new_unchecked(PROV_WAS_GENERATED_BY);

    let mut quads = vec![
        Quad::new(
            activity.clone(),
            NamedNode::new_unchecked(RDF_TYPE),
            NamedNode::new_unchecked(PROV_ACTIVITY),
            graph.clone(),
        ),
        Quad::new(
            activity.clone(),
            NamedNode::new_unchecked(PROV_WAS_ASSOCIATED_WITH),
            agent.clone(),
            graph.clone(),
        ),
        Quad::new(
            activity.clone(),
            NamedNode::new_unchecked(PROV_ENDED_AT_TIME),
            ts,
            graph.clone(),
        ),
        Quad::new(
            agent.clone(),
            NamedNode::new_unchecked(RDF_TYPE),
            NamedNode::new_unchecked(PROV_SOFTWARE_AGENT),
            graph.clone(),
        ),
        Quad::new(
            agent,
            NamedNode::new_unchecked(RDFS_LABEL),
            Literal::new_simple_literal(REASONER_AGENT),
            graph.clone(),
        ),
    ];

    for q in delta {
        let reifier = NamedNode::new_unchecked(format!(
            "https://moosedev.dev/kg/Reifier/{}",
            uuid::Uuid::new_v4()
        ));
        let triple_term = Term::Triple(Box::new(Triple::new(
            q.subject.clone(),
            q.predicate.clone(),
            q.object.clone(),
        )));
        quads.push(Quad::new(
            reifier.clone(),
            reifies.clone(),
            triple_term,
            graph.clone(),
        ));
        quads.push(Quad::new(
            reifier,
            was_generated_by.clone(),
            activity.clone(),
            graph.clone(),
        ));
    }
    quads
}

/// Remove every triple previously materialized by the reasoner from `data_graph_iri`,
/// along with its reification provenance in the provenance graph. Inferred triples are
/// recovered from the reifiers whose activity is `prov:wasAssociatedWith` the reasoner
/// agent. Idempotent (a no-op when nothing was materialized). Returns the count of
/// data-graph triples removed.
pub fn clear_reasoner_inferences(store: &Store, data_graph_iri: &str) -> anyhow::Result<usize> {
    let prov = GraphNameRef::NamedNode(NamedNodeRef::new_unchecked(PROVENANCE_GRAPH_IRI));
    let data_graph = GraphName::NamedNode(nn(data_graph_iri)?);
    let agent = NamedNode::new_unchecked(agent_iri(REASONER_AGENT));

    // Reasoner-inference activities = those associated with the reasoner agent.
    let activities: Vec<NamedNode> = store
        .quads_for_pattern(
            None,
            Some(NamedNodeRef::new_unchecked(PROV_WAS_ASSOCIATED_WITH)),
            Some(agent.as_ref().into()),
            Some(prov),
        )
        .flatten()
        .filter_map(|q| match q.subject {
            NamedOrBlankNode::NamedNode(n) => Some(n),
            _ => None,
        })
        .collect();

    let was_generated_by = NamedNodeRef::new_unchecked(PROV_WAS_GENERATED_BY);
    let mut data_removals: Vec<Quad> = Vec::new();
    let mut prov_removals: Vec<Quad> = Vec::new();

    for activity in &activities {
        // Reifiers generated by this activity.
        let reifiers: Vec<NamedOrBlankNode> = store
            .quads_for_pattern(
                None,
                Some(was_generated_by),
                Some(activity.as_ref().into()),
                Some(prov),
            )
            .flatten()
            .map(|q| q.subject)
            .collect();
        for r in &reifiers {
            for q in store
                .quads_for_pattern(Some(r.as_ref()), None, None, Some(prov))
                .flatten()
            {
                if q.predicate.as_str() == RDF_REIFIES {
                    if let Term::Triple(t) = &q.object {
                        data_removals.push(Quad::new(
                            t.subject.clone(),
                            t.predicate.clone(),
                            t.object.clone(),
                            data_graph.clone(),
                        ));
                    }
                }
                prov_removals.push(q);
            }
        }
        // The activity's own quads.
        for q in store
            .quads_for_pattern(Some(activity.as_ref().into()), None, None, Some(prov))
            .flatten()
        {
            prov_removals.push(q);
        }
    }

    let removed = data_removals.len();
    if data_removals.is_empty() && prov_removals.is_empty() {
        return Ok(0);
    }
    let mut txn = store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("clear_reasoner_inferences transaction: {e}"))?;
    for q in data_removals.iter().chain(prov_removals.iter()) {
        txn.remove(q.as_ref());
    }
    txn.commit()
        .map_err(|e| anyhow::anyhow!("clear_reasoner_inferences commit: {e}"))?;
    Ok(removed)
}
