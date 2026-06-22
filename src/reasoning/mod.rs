//! GROWL OWL 2 RL enrichment (app layer).
//!
//! Mirrors `trivyn/src/rdf/reasoning.rs`: marshals oxigraph quads into GROWL's term
//! model, runs the reasoner in **enrich** mode (`ReasonerConfig::enrich(true)` — property
//! and subclass propagation: prp-inv inverses, prp-spo subproperty, prp-dom/rng,
//! cax-sco; skips sameAs and class expressions), and returns the genuinely-inferred
//! delta. This module performs **no writes** — placement (co-locate in `kg/project`) and
//! per-triple provenance live in the write path.
//!
//! Enrichment can only materialize what the ontology DECLARES (Constraint `72d7a908`), so
//! callers MUST include the domain ontology T-box graphs (which carry `owl:inverseOf`,
//! `rdfs:subPropertyOf`, `rdfs:domain`/`range`) alongside the A-box data graph.

use chrono::{DateTime, Utc};
use growl::{OwnedReasonerResult, OwnedTerm, Reasoner, ReasonerConfig, Term as GrowlTerm};
use oxigraph::model::{
    BlankNode, GraphName, GraphNameRef, Literal, NamedNode, NamedNodeRef, NamedOrBlankNode, Quad,
    Term,
};
use oxigraph::store::Store;
use std::collections::{HashMap, HashSet};

// Reasoner arena: 32 MB floor, 1 KB per input triple — generous for enrich mode
// (trivyn/src/rdf/reasoning.rs).
const ARENA_MIN_BYTES: usize = 32 * 1024 * 1024;
const ARENA_BYTES_PER_TRIPLE: usize = 1024;

/// Bidirectional mapping between oxigraph blank-node string IDs and GROWL `i64` IDs.
struct BlankNodeMapper {
    to_growl: HashMap<String, i64>,
    to_oxigraph: HashMap<i64, String>,
    next_id: i64,
}

impl BlankNodeMapper {
    fn new() -> Self {
        Self {
            to_growl: HashMap::new(),
            to_oxigraph: HashMap::new(),
            next_id: 1,
        }
    }

    fn growl_id(&mut self, ox_id: &str) -> i64 {
        if let Some(&id) = self.to_growl.get(ox_id) {
            return id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.to_growl.insert(ox_id.to_string(), id);
        self.to_oxigraph.insert(id, ox_id.to_string());
        id
    }

    fn to_oxigraph_bnode(&self, growl_id: i64) -> BlankNode {
        match self.to_oxigraph.get(&growl_id) {
            Some(ox_id) => BlankNode::new_unchecked(ox_id),
            None => BlankNode::default(), // fresh blank node minted by inference
        }
    }
}

/// oxigraph subject -> borrowed GROWL term (zero-alloc).
fn subject_to_growl_term<'a>(s: &'a NamedOrBlankNode, m: &mut BlankNodeMapper) -> GrowlTerm<'a> {
    match s {
        NamedOrBlankNode::NamedNode(n) => GrowlTerm::Iri(n.as_str()),
        NamedOrBlankNode::BlankNode(b) => GrowlTerm::Blank(m.growl_id(b.as_str())),
    }
}

/// oxigraph object -> borrowed GROWL term (zero-alloc). Quoted-triple objects (rdf-12)
/// don't occur in the A-box/T-box we feed enrich, so they map to a sentinel IRI.
fn object_to_growl_term<'a>(o: &'a Term, m: &mut BlankNodeMapper) -> GrowlTerm<'a> {
    match o {
        Term::NamedNode(n) => GrowlTerm::Iri(n.as_str()),
        Term::BlankNode(b) => GrowlTerm::Blank(m.growl_id(b.as_str())),
        Term::Literal(lit) => GrowlTerm::Literal {
            value: lit.value(),
            datatype: Some(lit.datatype().as_str()),
            lang: lit.language(),
        },
        _ => GrowlTerm::Iri("urn:moosedev:unsupported:quoted-triple"),
    }
}

/// GROWL owned term -> oxigraph subject (literals can't be subjects).
fn owned_term_to_subject(t: &OwnedTerm, m: &BlankNodeMapper) -> Option<NamedOrBlankNode> {
    match t {
        OwnedTerm::Iri(s) => NamedNode::new(s).ok().map(NamedOrBlankNode::NamedNode),
        OwnedTerm::Blank(id) => Some(NamedOrBlankNode::BlankNode(m.to_oxigraph_bnode(*id))),
        OwnedTerm::Literal { .. } => None,
    }
}

/// GROWL owned term -> oxigraph term.
fn owned_term_to_term(t: &OwnedTerm, m: &BlankNodeMapper) -> Option<Term> {
    match t {
        OwnedTerm::Iri(s) => NamedNode::new(s).ok().map(Term::NamedNode),
        OwnedTerm::Blank(id) => Some(Term::BlankNode(m.to_oxigraph_bnode(*id))),
        OwnedTerm::Literal {
            value,
            datatype,
            lang,
        } => {
            let lit = if let Some(l) = lang {
                Literal::new_language_tagged_literal_unchecked(value, l)
            } else if let Some(dt) = datatype {
                match NamedNode::new(dt) {
                    Ok(dt_node) => Literal::new_typed_literal(value, dt_node),
                    Err(_) => Literal::new_simple_literal(value),
                }
            } else {
                Literal::new_simple_literal(value)
            };
            Some(Term::Literal(lit))
        }
    }
}

/// Run GROWL enrich over the A-box `data_graph_iri` plus the ontology T-box graphs, and
/// return the genuinely-inferred quads (the materialized closure minus the input), each
/// placed in `data_graph_iri`. Performs **no** writes.
///
/// The ontology graphs MUST carry the reasoning axioms (`owl:inverseOf`,
/// `rdfs:subPropertyOf`, `rdfs:domain`/`range`) — enrich materializes only what is
/// declared, so an empty/axiom-free ontology yields an empty delta.
///
/// The delta is **scoped to record→record object-property edges**: an inferred triple is
/// kept only when BOTH its subject and object are asserted nodes of `data_graph_iri`.
/// This discards T-box-internal closure (CCO/BFO entailments among ontology terms pulled
/// in as background), datatype-property liftings (literal objects), and `rdf:type`/class
/// completions — leaving exactly the inverse/subproperty edges between records that
/// bidirectional traversal follows (the comprehension-debt win, Requirement `b3ba8285`).
pub fn enrich_delta(
    store: &Store,
    data_graph_iri: &str,
    ontology_graph_iris: &[&str],
) -> anyhow::Result<Vec<Quad>> {
    // 1. Gather input quads from the data graph + each ontology graph.
    let mut graph_iris: Vec<&str> = Vec::with_capacity(1 + ontology_graph_iris.len());
    graph_iris.push(data_graph_iri);
    graph_iris.extend_from_slice(ontology_graph_iris);

    // Subjects asserted in the DATA graph (the A-box). The delta is scoped to edges FROM
    // these nodes, so T-box-internal entailments (CCO/BFO closure among ontology terms)
    // are discarded — we only materialize edges about project records.
    let mut abox_subjects: HashSet<String> = HashSet::new();
    let mut input: Vec<Quad> = Vec::new();
    for (idx, &iri) in graph_iris.iter().enumerate() {
        let g = NamedNodeRef::new(iri)?;
        for q in store.quads_for_pattern(None, None, None, Some(GraphNameRef::NamedNode(g))) {
            let quad = q?;
            // Skip RDF-1.2 reification bookkeeping (annotation triple terms in the
            // ontology's axiom annotations) — not OWL 2 RL input, only noise.
            if matches!(quad.object, Term::Triple(_)) {
                continue;
            }
            if idx == 0 {
                abox_subjects.insert(quad.subject.to_string());
            }
            input.push(quad);
        }
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }

    // 2. String-keyed input set so we keep only triples the reasoner DERIVED — not echoes
    //    of the asserted A-box or the ontology axioms copied through.
    let input_set: HashSet<(String, String, String)> = input
        .iter()
        .map(|q| {
            (
                q.subject.to_string(),
                q.predicate.to_string(),
                q.object.to_string(),
            )
        })
        .collect();

    // 3. Load into the reasoner and run enrich mode.
    let arena_bytes = std::cmp::max(ARENA_MIN_BYTES, input.len() * ARENA_BYTES_PER_TRIPLE);
    let mut reasoner = Reasoner::with_capacity(arena_bytes).filter_annotations(true);
    let mut mapper = BlankNodeMapper::new();
    for q in &input {
        let s = subject_to_growl_term(&q.subject, &mut mapper);
        let p = GrowlTerm::Iri(q.predicate.as_str());
        let o = object_to_growl_term(&q.object, &mut mapper);
        reasoner.add_triple_ref(&s, &p, &o);
    }

    let config = ReasonerConfig::new().verbose(false).enrich(true);
    let materialized = match reasoner.reason_with_config(&config) {
        OwnedReasonerResult::Success { triples, .. }
        | OwnedReasonerResult::Cancelled { triples, .. } => triples,
        OwnedReasonerResult::Inconsistent { reports } => {
            let reason = reports
                .first()
                .map(|r| r.reason.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("GROWL reports the enrichment input inconsistent: {reason}");
        }
    };

    // 4. Convert back, dropping echoes of the input — the remainder is the delta, placed
    //    in the data graph.
    let data_graph = GraphName::NamedNode(NamedNode::new(data_graph_iri)?);
    let mut delta = Vec::new();
    for t in &materialized {
        let Some(subject) = owned_term_to_subject(&t.subject, &mapper) else {
            continue;
        };
        // A-box edges only: the subject must be an asserted node of the data graph.
        let subject_str = subject.to_string();
        if !abox_subjects.contains(&subject_str) {
            continue;
        }
        let predicate = match &t.predicate {
            OwnedTerm::Iri(p) => match NamedNode::new(p) {
                Ok(n) => n,
                Err(_) => continue,
            },
            _ => continue,
        };
        let Some(object) = owned_term_to_term(&t.object, &mapper) else {
            continue;
        };
        // Walk-relevant edges only: the object must also be an A-box record. Drops
        // datatype-property liftings (literal objects) and rdf:type / T-box-class
        // completions, leaving the record->record object-property edges that
        // bidirectional traversal (record_neighbors) actually follows.
        if !matches!(&object, Term::NamedNode(_)) || !abox_subjects.contains(&object.to_string()) {
            continue;
        }
        let key = (subject_str, predicate.to_string(), object.to_string());
        if input_set.contains(&key) {
            continue;
        }
        delta.push(Quad::new(subject, predicate, object, data_graph.clone()));
    }
    Ok(delta)
}

/// Materialize the inferred A-box delta into `data_graph_iri` and record its provenance,
/// dropping any prior reasoner output first so the operation is **idempotent**
/// (drop-and-rerun). Inferred edges are co-located in the data graph (so the existing
/// kg/project-scoped retrieval reads traverse them) and each is reified into the
/// provenance graph (`R rdf:reifies «s p o» ; prov:wasGeneratedBy <activity>`) — the
/// per-triple tag that makes the next drop precise. Returns the number of edges
/// materialized.
///
/// Writes the inferred edges directly via a store transaction (NOT `index_record`): they
/// carry no `rdfs:label`/`hasDescription`, so the dense instance index has nothing to
/// embed and stays coherent (the direct-write/stale-index footgun does not apply here).
pub fn enrich(
    store: &Store,
    data_graph_iri: &str,
    ontology_graph_iris: &[&str],
    when: DateTime<Utc>,
) -> anyhow::Result<usize> {
    crate::provenance::clear_reasoner_inferences(store, data_graph_iri)?;
    let delta = enrich_delta(store, data_graph_iri, ontology_graph_iris)?;
    if delta.is_empty() {
        return Ok(0);
    }
    let provenance = crate::provenance::reasoner_inference_quads(&delta, when);

    let mut txn = store
        .start_transaction()
        .map_err(|e| anyhow::anyhow!("enrich transaction: {e}"))?;
    txn.extend(delta.iter().map(Quad::as_ref));
    txn.extend(provenance.iter().map(Quad::as_ref));
    txn.commit()
        .map_err(|e| anyhow::anyhow!("enrich commit: {e}"))?;
    Ok(delta.len())
}

/// Convenience wrapper stamping the enrichment with the current time.
pub fn enrich_now(
    store: &Store,
    data_graph_iri: &str,
    ontology_graph_iris: &[&str],
) -> anyhow::Result<usize> {
    enrich(store, data_graph_iri, ontology_graph_iris, Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// prp-inv1: a declared `owl:inverseOf` materializes the reverse edge, placed in the
    /// data graph, and the asserted forward edge is NOT echoed into the delta.
    #[test]
    fn enrich_materializes_declared_inverse() -> anyhow::Result<()> {
        let store = Store::new()?;
        let data = NamedNodeRef::new("urn:test:data")?;
        let onto = NamedNodeRef::new("urn:test:onto")?;

        let a = NamedNode::new("urn:test:a")?;
        let b = NamedNode::new("urn:test:b")?;
        let concerns = NamedNode::new("urn:test:concerns")?;
        let is_concerned_by = NamedNode::new("urn:test:isConcernedBy")?;
        let inverse_of = NamedNode::new("http://www.w3.org/2002/07/owl#inverseOf")?;
        let rdf_type = NamedNode::new("http://www.w3.org/1999/02/22-rdf-syntax-ns#type")?;
        let object_property = NamedNode::new("http://www.w3.org/2002/07/owl#ObjectProperty")?;
        let label = NamedNode::new("http://www.w3.org/2000/01/rdf-schema#label")?;

        // A-box: a concerns b. Both carry a label, so both are asserted A-box subjects
        // (as every real record is) and survive the A-box-subject delta scoping.
        store.insert(&Quad::new(a.clone(), concerns.clone(), b.clone(), data))?;
        store.insert(&Quad::new(a.clone(), label.clone(), Literal::new_simple_literal("a"), data))?;
        store.insert(&Quad::new(b.clone(), label, Literal::new_simple_literal("b"), data))?;
        // T-box: concerns owl:inverseOf isConcernedBy (+ ObjectProperty typing).
        store.insert(&Quad::new(
            concerns.clone(),
            inverse_of,
            is_concerned_by.clone(),
            onto,
        ))?;
        store.insert(&Quad::new(
            concerns.clone(),
            rdf_type.clone(),
            object_property.clone(),
            onto,
        ))?;
        store.insert(&Quad::new(
            is_concerned_by.clone(),
            rdf_type,
            object_property,
            onto,
        ))?;

        let delta = enrich_delta(&store, "urn:test:data", &["urn:test:onto"])?;

        let expected = Quad::new(
            b.clone(),
            is_concerned_by.clone(),
            a.clone(),
            GraphName::NamedNode(NamedNode::new("urn:test:data")?),
        );
        assert!(
            delta.contains(&expected),
            "expected materialized inverse {expected} in delta, got: {delta:?}"
        );
        // The asserted forward edge is excluded from the delta (input dedup).
        assert!(
            !delta.iter().any(|q| q.predicate == concerns),
            "delta should not echo the asserted `concerns` edge: {delta:?}"
        );
        Ok(())
    }

    /// `enrich` co-locates the inferred edge in the data graph, reifies it in the
    /// provenance graph, and is idempotent across re-runs (drop-and-rerun).
    #[test]
    fn enrich_writes_and_is_idempotent() -> anyhow::Result<()> {
        use crate::provenance::PROVENANCE_GRAPH_IRI;

        let store = Store::new()?;
        let data_iri = "urn:test:data";
        let data = NamedNodeRef::new(data_iri)?;
        let onto = NamedNodeRef::new("urn:test:onto")?;

        let a = NamedNode::new("urn:test:a")?;
        let b = NamedNode::new("urn:test:b")?;
        let concerns = NamedNode::new("urn:test:concerns")?;
        let is_concerned_by = NamedNode::new("urn:test:isConcernedBy")?;
        let inverse_of = NamedNode::new("http://www.w3.org/2002/07/owl#inverseOf")?;
        let label = NamedNode::new("http://www.w3.org/2000/01/rdf-schema#label")?;

        store.insert(&Quad::new(a.clone(), concerns.clone(), b.clone(), data))?;
        store.insert(&Quad::new(
            a.clone(),
            label.clone(),
            Literal::new_simple_literal("a"),
            data,
        ))?;
        store.insert(&Quad::new(
            b.clone(),
            label,
            Literal::new_simple_literal("b"),
            data,
        ))?;
        store.insert(&Quad::new(
            concerns.clone(),
            inverse_of,
            is_concerned_by.clone(),
            onto,
        ))?;

        let count_inverse = || -> usize {
            store
                .quads_for_pattern(
                    None,
                    Some(is_concerned_by.as_ref()),
                    None,
                    Some(GraphNameRef::NamedNode(NamedNodeRef::new(data_iri).unwrap())),
                )
                .count()
        };
        let reifies = NamedNode::new("http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies")?;
        let count_reifies = || -> usize {
            store
                .quads_for_pattern(
                    None,
                    Some(reifies.as_ref()),
                    None,
                    Some(GraphNameRef::NamedNode(
                        NamedNodeRef::new(PROVENANCE_GRAPH_IRI).unwrap(),
                    )),
                )
                .count()
        };

        let n = enrich(&store, data_iri, &["urn:test:onto"], Utc::now())?;
        assert_eq!(n, 1, "one inverse edge materialized");
        assert_eq!(count_inverse(), 1, "inverse co-located in the data graph");
        assert_eq!(count_reifies(), 1, "inverse reified in the provenance graph");

        // Re-run: drop-and-rerun keeps exactly one inverse edge + one reifier (idempotent).
        let n2 = enrich(&store, data_iri, &["urn:test:onto"], Utc::now())?;
        assert_eq!(n2, 1);
        assert_eq!(count_inverse(), 1, "no edge duplication after re-enrich");
        assert_eq!(count_reifies(), 1, "no provenance duplication after re-enrich");
        Ok(())
    }
}
