//! Linked evidence: the governing knowledge a deterministic structural walk
//! reaches from the code of a set of files.
//!
//! The harness push leads with this walk instead of similarity-ranked topic
//! recall (AD 85da8700). From the records linked to the files' code it follows
//! typed edges the graph already holds: accepted rules (Constraints and
//! Requirements) on the components that code belongs to, the records those
//! records are motivated by, the current heads of supersession chains, and the
//! Lessons learned from them.
//! Records whose claims the file dossiers already print are excluded, so the
//! walk adds what the dossiers cannot carry. Every hop reads both the canonical
//! and the inverse edge, so the walk does not depend on inference having run.

use std::collections::{BTreeMap, BTreeSet};

use oxigraph::model::NamedNode;

use super::capture::asserted_project_types;
use super::code_entities::{file_entity_iris, CodeTerms};
use super::components::{best_component_for_path, load_components};
use super::context::{context_item_for_iri, first_literal, render_claim_body};
use super::dossier::{
    collect_records, direct_records_for_entity, linked_records, realized_components, sort_records,
    summarize_record, CanonicalDirection, LinkPairs, PredicatePair, RecordSummary,
};
use super::lifecycle::{in_working_set, is_accepted};
use super::state::AppState;
use super::util::{datatype_property_iri, local_name};

/// The record kinds the harness delivers as governing rules, in delivery
/// priority. Constraints lead: Constraint 927d5176 protects their claims, so
/// every budget below must reach them before it reaches anything else.
pub const RULE_KINDS: [&str; 2] = ["Constraint", "Requirement"];

/// Whether a record governs the code it is linked to, and so belongs in the
/// Project rules block rather than in linked evidence.
pub fn is_rule_kind(kind: &str) -> bool {
    RULE_KINDS.contains(&kind)
}

/// Delivery priority of a rule kind; unknown kinds sort last.
fn rule_rank(kind: &str) -> usize {
    RULE_KINDS
        .iter()
        .position(|known| *known == kind)
        .unwrap_or(RULE_KINDS.len())
}

/// Rules of one kind rendered with complete claims, counted across every hop;
/// later rules of that kind render their header only. Per kind rather than
/// overall, so a graph with more than this many Constraints cannot leave every
/// Requirement nameless. A rule is never dropped.
const RULE_CLAIM_LIMIT: usize = 24;

/// Bytes of rule claims the Project rules block carries in full, spent
/// Constraints first. It bounds only the kinds admitted after Constraints:
/// Constraint 927d5176 rule 4 guarantees accepted Constraints their claims up
/// to [`RULE_CLAIM_LIMIT`], so this budget can never take one. The runner
/// journals what was delivered, because a rule reduced to its name is not
/// delivered at all (Lesson f07aacbb).
const RULE_CLAIM_BYTES: usize = 16 * 1024;

/// How the walk reached a record. The order is the dedup priority: a record
/// reached by several hops is shown once, under the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Hop {
    /// An accepted rule linked directly to the files' code. Never a walk
    /// candidate (the dossiers print its claim); it names governing rules.
    Direct,
    /// An accepted rule on a component the files' code belongs to.
    Component,
    /// A record a directly linked record is motivated by.
    Motivating,
    /// The current head of a chain that supersedes a directly linked record.
    SupersessionHead,
    /// A Lesson learned from a directly linked record.
    Lesson,
}

impl Hop {
    /// The `via:` line naming what the walk reached the record from.
    pub(crate) fn via(self, source: &str) -> String {
        match self {
            Hop::Direct => format!("via: linked to {source}"),
            Hop::Component => format!("via: component {source}"),
            Hop::Motivating => format!("via: motivates {source}"),
            Hop::SupersessionHead => format!("via: supersedes {source}"),
            Hop::Lesson => format!("via: learned from {source}"),
        }
    }

    /// Non-rule records rendered in full per hop before the rest are omitted.
    fn limit(self) -> usize {
        match self {
            Hop::Direct | Hop::Component => usize::MAX,
            Hop::Motivating => 8,
            Hop::SupersessionHead => 8,
            Hop::Lesson => 6,
        }
    }
}

/// One record the walk delivers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedRecord {
    pub iri: String,
    pub kind: String,
    /// The record's `rdfs:label`, as topic evidence renders it.
    pub label: String,
    pub hop: Hop,
    /// What the walk reached the record from: a component label or the title of
    /// the directly linked record.
    pub source: String,
    /// The claim body, rendered by the shared claim renderer.
    pub claim: String,
}

/// The walk's result for one set of files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkedEvidence {
    /// Selected records in hop order, each shown once.
    pub records: Vec<LinkedRecord>,
    /// Working-set records linked directly to the files' code. The file dossiers
    /// print their claims, so the walk and any fallback leave them out.
    pub excluded: BTreeSet<String>,
    /// Accepted rules among the directly linked records, with claims, in
    /// dossier order. Together with the rules the walk reached they are the
    /// governing rules of the files.
    pub direct_rules: Vec<LinkedRecord>,
}

/// Walk from the code of `files` to the governing knowledge linked around it.
///
/// Direct records are the dossier's: every record linked to the files' code
/// entities (whole-file module and definitions). Components are the union of
/// what the entities realize, what the files' paths are declared under, and the
/// components the direct records concern or constrain.
pub fn linked_evidence(state: &AppState, files: &[String]) -> anyhow::Result<LinkedEvidence> {
    let terms = CodeTerms::resolve(state)?;
    let pairs = LinkPairs::resolve(state)?;
    let catalog = load_components(state)?;
    let mut direct: BTreeMap<String, RecordSummary> = BTreeMap::new();
    let mut direct_file: BTreeMap<String, String> = BTreeMap::new();
    let mut components: BTreeSet<String> = BTreeSet::new();
    for file in files {
        for entity in file_entity_iris(state, file)? {
            for record in direct_records_for_entity(state, &entity)? {
                direct_file
                    .entry(record.iri.clone())
                    .or_insert_with(|| file.clone());
                direct.entry(record.iri.clone()).or_insert(record);
            }
            components.extend(realized_components(state, &terms, &entity)?);
        }
        if let Some(iri) = best_component_for_path(file, &catalog).and_then(|c| c.iri.clone()) {
            components.insert(iri);
        }
    }
    let sources: Vec<&RecordSummary> = direct
        .values()
        .filter(|record| in_working_set(&record.status))
        .collect();
    let excluded: BTreeSet<String> = sources.iter().map(|record| record.iri.clone()).collect();
    let record_to_component = [
        PredicatePair::new(
            state,
            "concerns",
            "isConcernedBy",
            CanonicalDirection::EntityToRecord,
        )?,
        PredicatePair::new(
            state,
            "constrains",
            "isConstrainedBy",
            CanonicalDirection::EntityToRecord,
        )?,
    ];
    let system_component = state.resolve_class("SystemComponent")?;
    for record in &sources {
        for pair in &record_to_component {
            for iri in linked_records(state, pair, &record.iri)? {
                let is_component = NamedNode::new(iri.as_str()).is_ok_and(|node| {
                    asserted_project_types(state, &node).contains(&system_component)
                });
                if is_component {
                    components.insert(iri);
                }
            }
        }
    }

    let mut candidates = Vec::new();
    for component in &components {
        let label = component_label(state, component);
        let mut rules: Vec<RecordSummary> = collect_records(state, &pairs.component, component)?
            .into_iter()
            .filter(|record| is_rule_kind(&record.kind) && is_accepted(&record.status))
            .collect();
        sort_records(&mut rules);
        push_candidates(&mut candidates, rules, Hop::Component, &label);
    }

    let motivating = PredicatePair::new(
        state,
        "isMotivatedBy",
        "motivates",
        CanonicalDirection::EntityToRecord,
    )?;
    for record in &sources {
        let found = accepted_records(
            state,
            linked_records(state, &motivating, &record.iri)?,
            "isMotivatedBy",
            None,
        );
        push_candidates(&mut candidates, found, Hop::Motivating, &record.title);
    }

    let successor = PredicatePair::new(
        state,
        "isSupersededBy",
        "supersedes",
        CanonicalDirection::EntityToRecord,
    )?;
    let status =
        |iri: &str| first_literal(&state.store, iri, &state.capture.status).unwrap_or_default();
    for record in direct
        .values()
        .filter(|record| record.status.eq_ignore_ascii_case("superseded"))
    {
        let mut successors = |iri: &str| -> anyhow::Result<Vec<String>> {
            Ok(linked_records(state, &successor, iri)?
                .into_iter()
                .collect())
        };
        if let Some(head) = supersession_head(&record.iri, &mut successors, &status)? {
            let found = accepted_records(state, BTreeSet::from([head]), "supersedes", None);
            push_candidates(&mut candidates, found, Hop::SupersessionHead, &record.title);
        }
    }

    let learned = PredicatePair::new(
        state,
        "learnedFrom",
        "yieldsLesson",
        CanonicalDirection::RecordToEntity,
    )?;
    for record in &sources {
        let found = accepted_records(
            state,
            linked_records(state, &learned, &record.iri)?,
            "learnedFrom",
            Some("Lesson"),
        );
        push_candidates(&mut candidates, found, Hop::Lesson, &record.title);
    }

    let mut records = select(candidates, &excluded);
    records.retain_mut(|record| {
        let Some(item) = context_item_for_iri(state, &record.iri, false) else {
            return false;
        };
        record.label = item.label.clone();
        record.kind = item.kind.clone();
        render_claim_body(&item, &mut record.claim);
        true
    });
    let mut rules: Vec<RecordSummary> = sources
        .iter()
        .filter(|record| is_rule_kind(&record.kind) && is_accepted(&record.status))
        .map(|record| (*record).clone())
        .collect();
    sort_records(&mut rules);
    let direct_rules = rules
        .into_iter()
        .filter_map(|record| {
            let item = context_item_for_iri(state, &record.iri, false)?;
            let mut claim = String::new();
            render_claim_body(&item, &mut claim);
            Some(LinkedRecord {
                source: direct_file.get(&record.iri).cloned().unwrap_or_default(),
                iri: record.iri,
                kind: item.kind.clone(),
                label: item.label.clone(),
                hop: Hop::Direct,
                claim,
            })
        })
        .collect();
    Ok(LinkedEvidence {
        records,
        excluded,
        direct_rules,
    })
}

/// Render the walk's records: header, `via:` line and claim body per record,
/// then one line counting what the caps left out. Every rule is listed; past
/// [`RULE_CLAIM_LIMIT`] for its kind a rule renders without its claim. Other
/// records past their hop's limit are omitted and counted.
pub fn render_linked_evidence(records: &[LinkedRecord]) -> String {
    let mut out = String::new();
    let mut rules_per_kind = BTreeMap::<&str, usize>::new();
    let mut rendered_per_hop = BTreeMap::<Hop, usize>::new();
    let mut not_in_full = BTreeMap::<&str, usize>::new();
    for record in records {
        let is_rule = is_rule_kind(&record.kind);
        let in_full = if is_rule {
            let rendered = rules_per_kind.entry(record.kind.as_str()).or_default();
            *rendered += 1;
            *rendered <= RULE_CLAIM_LIMIT
        } else {
            let rendered = rendered_per_hop.entry(record.hop).or_default();
            let fits = *rendered < record.hop.limit();
            *rendered += usize::from(fits);
            fits
        };
        if !in_full {
            *not_in_full.entry(&record.kind).or_default() += 1;
            if !is_rule {
                continue;
            }
        }
        out.push_str(&format!(
            "\n[{}] {} ({})\n{}\n",
            record.kind,
            record.label,
            record.iri,
            record.hop.via(&record.source)
        ));
        if in_full {
            out.push_str(&record.claim);
        }
    }
    let count: usize = not_in_full.values().sum();
    if count > 0 {
        let noun = if count == 1 { "record" } else { "records" };
        let kinds = not_in_full
            .iter()
            .map(|(kind, n)| format!("{kind}: {n}"))
            .collect::<Vec<_>>()
            .join("; ");
        out.push_str(&format!(
            "\n{count} further linked {noun} not shown in full ({kinds}); search project knowledge for their claims\n"
        ));
    }
    out
}

/// The line that replaces a governing rule's claim in linked evidence.
pub const RULE_POINTER: &str = "claim under Project rules\n";

/// The governing rules of the walked files: accepted rules linked directly to
/// their code, then the rules the walk reached, in hop order, and Constraints
/// ahead of Requirements.
///
/// Every rule is named. A claim is carried while its kind is within
/// [`RULE_CLAIM_LIMIT`] and the shared [`RULE_CLAIM_BYTES`] budget still holds
/// it; otherwise the claim is empty and the rule keeps its place. Constraints
/// are ordered first, so neither bound can take a Constraint's claim to make
/// room for a Requirement (Constraint 212a2026).
pub fn governing_rules(evidence: &LinkedEvidence) -> Vec<LinkedRecord> {
    let mut rules: Vec<LinkedRecord> = evidence
        .direct_rules
        .iter()
        .chain(
            evidence
                .records
                .iter()
                .filter(|record| is_rule_kind(&record.kind)),
        )
        .cloned()
        .collect();
    // Stable, so within a kind the direct-then-walked hop order is preserved.
    rules.sort_by_key(|rule| rule_rank(&rule.kind));
    let mut per_kind = BTreeMap::<String, usize>::new();
    let mut claim_bytes = 0usize;
    for rule in &mut rules {
        let rendered = per_kind.entry(rule.kind.clone()).or_default();
        *rendered += 1;
        // Constraint 927d5176 rule 4, which that amendment lists among what it
        // does not relax: accepted Constraints render with full claims up to 24
        // and title-only beyond, never omitted. The byte budget therefore binds
        // only the kinds admitted since, so nothing new can cost a Constraint
        // its claim. Constraints still spend from the budget, so a Requirement
        // sees what is genuinely left.
        let within_bytes =
            rule.kind == "Constraint" || claim_bytes + rule.claim.len() <= RULE_CLAIM_BYTES;
        let fits = *rendered <= RULE_CLAIM_LIMIT && within_bytes;
        if fits {
            claim_bytes += rule.claim.len();
        } else {
            rule.claim.clear();
        }
    }
    rules
}

/// How the Project rules block was delivered: rules of each kind, and how many
/// of them carry their claim. Journaled per step, because a rule delivered as a
/// name only is not delivered (Lesson f07aacbb) and that must be visible in the
/// receipt rather than inferred from the prompt.
pub fn rules_delivery(rules: &[(String, bool)]) -> String {
    let mut per_kind: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (kind, has_claim) in rules {
        let entry = per_kind.entry(kind.as_str()).or_default();
        entry.0 += 1;
        entry.1 += usize::from(*has_claim);
    }
    if per_kind.is_empty() {
        return "no governing rules".to_string();
    }
    per_kind
        .into_iter()
        .map(|(kind, (total, with_claim))| format!("{kind}: {total}, {with_claim} with claims"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The walk's records with each governing rule's claim replaced by a pointer
/// to the Project rules, which carry it.
pub fn with_rule_pointers(records: &[LinkedRecord]) -> Vec<LinkedRecord> {
    records
        .iter()
        .map(|record| {
            let mut record = record.clone();
            if is_rule_kind(&record.kind) {
                record.claim = RULE_POINTER.to_string();
            }
            record
        })
        .collect()
}

/// Candidates for one hop, in the order they were found.
fn push_candidates(
    out: &mut Vec<LinkedRecord>,
    records: Vec<RecordSummary>,
    hop: Hop,
    source: &str,
) {
    out.extend(records.into_iter().map(|record| LinkedRecord {
        iri: record.iri,
        kind: record.kind,
        label: String::new(),
        hop,
        source: source.to_string(),
        claim: String::new(),
    }));
}

/// Summaries of the accepted records among `iris`, optionally of one kind, in
/// dossier order.
fn accepted_records(
    state: &AppState,
    iris: BTreeSet<String>,
    predicate_local: &str,
    kind: Option<&str>,
) -> Vec<RecordSummary> {
    let mut out: Vec<RecordSummary> = iris
        .iter()
        .filter_map(|iri| summarize_record(state, iri, predicate_local))
        .filter(|record| is_accepted(&record.status) && kind.is_none_or(|kind| record.kind == kind))
        .collect();
    sort_records(&mut out);
    out
}

/// A component's display name: its label, component name, or title.
fn component_label(state: &AppState, iri: &str) -> String {
    first_literal(&state.store, iri, moose::RDFS_LABEL)
        .or_else(|| {
            datatype_property_iri(&state.arch_vocab, "hasComponentName")
                .ok()
                .and_then(|predicate| first_literal(&state.store, iri, &predicate))
        })
        .or_else(|| first_literal(&state.store, iri, &state.capture.title))
        .unwrap_or_else(|| local_name(iri).to_string())
}

/// Order candidates by hop, keep each record's first reach, and drop records
/// whose claims the dossiers already carry.
fn select(mut candidates: Vec<LinkedRecord>, excluded: &BTreeSet<String>) -> Vec<LinkedRecord> {
    candidates.sort_by_key(|candidate| candidate.hop);
    let mut seen = BTreeSet::new();
    candidates
        .into_iter()
        .filter(|candidate| {
            !excluded.contains(&candidate.iri) && seen.insert(candidate.iri.clone())
        })
        .collect()
}

/// Follow a chain of successors from a superseded record to its current head:
/// the first accepted record with no accepted successor. A chain that ends in a
/// deprecated, rejected or proposed record, or loops, has no head.
fn supersession_head(
    start: &str,
    successors: &mut dyn FnMut(&str) -> anyhow::Result<Vec<String>>,
    status: &dyn Fn(&str) -> String,
) -> anyhow::Result<Option<String>> {
    let mut visited = BTreeSet::from([start.to_string()]);
    let mut next = successors(start)?;
    next.sort();
    let mut stack: Vec<String> = next.into_iter().rev().collect();
    while let Some(iri) = stack.pop() {
        if !visited.insert(iri.clone()) {
            continue;
        }
        let current = status(&iri);
        let mut after = successors(&iri)?;
        after.sort();
        if is_accepted(&current) {
            let accepted_successor = after
                .iter()
                .any(|successor| !visited.contains(successor) && is_accepted(&status(successor)));
            if !accepted_successor {
                return Ok(Some(iri));
            }
        } else if !current.eq_ignore_ascii_case("superseded") {
            continue;
        }
        stack.extend(after.into_iter().rev());
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(iri: &str, kind: &str, hop: Hop, source: &str) -> LinkedRecord {
        LinkedRecord {
            iri: iri.into(),
            kind: kind.into(),
            label: format!("{iri} label"),
            hop,
            source: source.into(),
            claim: format!("hasDescription: claim of {iri}\n"),
        }
    }

    #[test]
    fn selection_orders_hops_keeps_the_first_reach_and_drops_dossier_records() {
        let excluded = BTreeSet::from(["urn:direct".to_string()]);
        let selected = select(
            vec![
                candidate("urn:lesson", "Lesson", Hop::Lesson, "Decision"),
                candidate("urn:np7", "Constraint", Hop::Component, "Billing"),
                candidate("urn:need", "Requirement", Hop::Motivating, "Decision"),
                candidate("urn:np7", "Constraint", Hop::Motivating, "Decision"),
                candidate("urn:np7", "Constraint", Hop::Component, "Fees"),
                candidate(
                    "urn:head",
                    "ArchitecturalDecision",
                    Hop::SupersessionHead,
                    "Old",
                ),
                candidate("urn:direct", "Requirement", Hop::Motivating, "Decision"),
            ],
            &excluded,
        );
        let reached: Vec<_> = selected
            .iter()
            .map(|record| (record.iri.as_str(), record.hop, record.source.as_str()))
            .collect();
        assert_eq!(
            reached,
            vec![
                ("urn:np7", Hop::Component, "Billing"),
                ("urn:need", Hop::Motivating, "Decision"),
                ("urn:head", Hop::SupersessionHead, "Old"),
                ("urn:lesson", Hop::Lesson, "Decision"),
            ]
        );
    }

    #[test]
    fn a_record_renders_its_header_via_line_and_claim() {
        let rendered = render_linked_evidence(&[candidate(
            "urn:np7",
            "Constraint",
            Hop::Component,
            "Billing",
        )]);
        assert_eq!(
            rendered,
            "\n[Constraint] urn:np7 label (urn:np7)\nvia: component Billing\nhasDescription: claim of urn:np7\n"
        );
    }

    #[test]
    fn governing_rules_list_direct_then_walked_constraints_and_cap_claims() {
        let mut evidence = LinkedEvidence {
            direct_rules: vec![candidate("urn:d", "Constraint", Hop::Direct, "src/a.rs")],
            ..Default::default()
        };
        for n in 0..26 {
            evidence.records.push(candidate(
                &format!("urn:c{n:02}"),
                "Constraint",
                Hop::Component,
                "Uploads",
            ));
        }
        evidence
            .records
            .push(candidate("urn:l", "Lesson", Hop::Lesson, "Decision"));
        let rules = governing_rules(&evidence);
        assert_eq!(rules.len(), 27, "every Constraint named, no Lesson");
        assert_eq!(rules[0].iri, "urn:d");
        assert_eq!(rules[1].iri, "urn:c00");
        assert_eq!(
            rules.iter().filter(|rule| !rule.claim.is_empty()).count(),
            24
        );
        assert!(rules[26].claim.is_empty());
        let pointed = with_rule_pointers(&evidence.records);
        assert_eq!(pointed[0].claim, RULE_POINTER);
        assert_eq!(pointed[26].claim, "hasDescription: claim of urn:l\n");
    }

    /// Requirements govern too, but never at a Constraint's expense: they sort
    /// behind, and the per-kind count reaches them on their own allowance
    /// rather than on what Constraints left over.
    #[test]
    fn requirements_govern_behind_constraints_and_keep_their_own_claim_allowance() {
        let mut evidence = LinkedEvidence {
            direct_rules: vec![candidate(
                "urn:req:direct",
                "Requirement",
                Hop::Direct,
                "src/a.rs",
            )],
            ..Default::default()
        };
        // More Constraints than one kind's claim allowance, so a shared cap
        // would leave every Requirement nameless — the f07aacbb failure.
        for n in 0..30 {
            evidence.records.push(candidate(
                &format!("urn:c{n:02}"),
                "Constraint",
                Hop::Component,
                "Uploads",
            ));
        }
        for n in 0..3 {
            evidence.records.push(candidate(
                &format!("urn:r{n}"),
                "Requirement",
                Hop::Component,
                "Uploads",
            ));
        }
        let rules = governing_rules(&evidence);
        let kinds: Vec<&str> = rules.iter().map(|rule| rule.kind.as_str()).collect();
        assert_eq!(
            kinds.iter().filter(|kind| **kind == "Constraint").count(),
            30
        );
        assert!(
            kinds[..30].iter().all(|kind| *kind == "Constraint"),
            "Constraints lead: {kinds:?}"
        );
        assert!(kinds[30..].iter().all(|kind| *kind == "Requirement"));
        // Every Constraint claim that fitted before still fits.
        let with_claims = |kind: &str| {
            rules
                .iter()
                .filter(|rule| rule.kind == kind && !rule.claim.is_empty())
                .count()
        };
        assert_eq!(with_claims("Constraint"), RULE_CLAIM_LIMIT);
        assert_eq!(
            with_claims("Requirement"),
            4,
            "all four, on their own count"
        );
        assert!(
            rules.iter().all(|rule| !rule.label.is_empty()),
            "none dropped"
        );
    }

    /// The byte budget bounds Requirements but never costs a Constraint its
    /// claim, which Constraint 927d5176 rule 4 guarantees.
    #[test]
    fn the_claim_byte_budget_bounds_requirements_and_never_a_constraint() {
        let long = |iri: &str, kind: &str| {
            let mut record = candidate(iri, kind, Hop::Component, "Uploads");
            record.claim = "x".repeat(RULE_CLAIM_BYTES / 2);
            record
        };
        let evidence = LinkedEvidence {
            records: vec![
                long("urn:r0", "Requirement"),
                long("urn:c0", "Constraint"),
                long("urn:c1", "Constraint"),
                long("urn:c2", "Constraint"),
                long("urn:r1", "Requirement"),
            ],
            ..Default::default()
        };
        let rules = governing_rules(&evidence);
        let carried: Vec<(&str, bool)> = rules
            .iter()
            .map(|rule| (rule.iri.as_str(), !rule.claim.is_empty()))
            .collect();
        assert_eq!(
            carried,
            vec![
                ("urn:c0", true),
                ("urn:c1", true),
                ("urn:c2", true),
                ("urn:r0", false),
                ("urn:r1", false),
            ],
            "every Constraint keeps its claim past the budget; the Requirements keep their names"
        );
    }

    #[test]
    fn the_delivery_receipt_counts_each_kind_and_its_claims() {
        assert_eq!(rules_delivery(&[]), "no governing rules");
        let delivered = vec![
            ("Constraint".to_string(), true),
            ("Constraint".to_string(), false),
            ("Requirement".to_string(), true),
        ];
        assert_eq!(
            rules_delivery(&delivered),
            "Constraint: 2, 1 with claims; Requirement: 1, 1 with claims"
        );
    }

    #[test]
    fn via_lines_name_the_direct_file_and_every_hop_source() {
        assert_eq!(Hop::Direct.via("src/fees.rs"), "via: linked to src/fees.rs");
        assert_eq!(Hop::Component.via("Billing"), "via: component Billing");
        assert!(Hop::Direct < Hop::Component, "direct rules sort first");
    }

    #[test]
    fn caps_keep_every_constraint_and_count_what_they_leave_out() {
        let mut records = Vec::new();
        for n in 0..30 {
            records.push(candidate(
                &format!("urn:c{n:02}"),
                "Constraint",
                Hop::Component,
                "Billing",
            ));
        }
        for n in 0..9 {
            records.push(candidate(
                &format!("urn:m{n}"),
                "ArchitecturalDecision",
                Hop::Motivating,
                "Decision",
            ));
        }
        records.push(candidate(
            "urn:mc",
            "Constraint",
            Hop::Motivating,
            "Decision",
        ));
        for n in 0..10 {
            records.push(candidate(
                &format!("urn:l{n}"),
                "Lesson",
                Hop::Lesson,
                "Decision",
            ));
        }
        let rendered = render_linked_evidence(&records);
        assert_eq!(rendered, render_linked_evidence(&records), "deterministic");
        // Every Constraint is listed; the first 24 by kind carry their claims,
        // including one reached through the motivating hop.
        for n in 0..30 {
            let iri = format!("urn:c{n:02}");
            assert!(rendered.contains(&format!("({iri})\n")), "{iri} listed");
            assert_eq!(
                rendered.contains(&format!("claim of {iri}\n")),
                n < 24,
                "{iri} claim"
            );
        }
        assert!(rendered.contains("(urn:mc)\nvia: motivates Decision\n"));
        assert!(
            !rendered.contains("claim of urn:mc\n"),
            "25th Constraint is title-only"
        );
        // Eight motivating decisions and six Lessons render; the rest are omitted.
        for n in 0..9 {
            assert_eq!(rendered.contains(&format!("(urn:m{n})")), n < 8);
        }
        for n in 0..10 {
            assert_eq!(rendered.contains(&format!("(urn:l{n})")), n < 6);
        }
        assert!(rendered.ends_with(
            "\n12 further linked records not shown in full (ArchitecturalDecision: 1; Constraint: 7; Lesson: 4); search project knowledge for their claims\n"
        ), "{rendered}");
    }

    #[test]
    fn supersession_heads_follow_the_chain_and_stop_at_loops_and_retired_ends() {
        let edges: BTreeMap<&str, Vec<&str>> = BTreeMap::from([
            ("urn:a", vec!["urn:b"]),
            ("urn:b", vec!["urn:c"]),
            ("urn:loop1", vec!["urn:loop2"]),
            ("urn:loop2", vec!["urn:loop1"]),
            ("urn:old", vec!["urn:dead"]),
            ("urn:pending-base", vec!["urn:current"]),
            ("urn:current", vec!["urn:proposal"]),
        ]);
        let statuses: BTreeMap<&str, &str> = BTreeMap::from([
            ("urn:a", "superseded"),
            ("urn:b", "superseded"),
            ("urn:c", "accepted"),
            ("urn:loop1", "superseded"),
            ("urn:loop2", "superseded"),
            ("urn:old", "superseded"),
            ("urn:dead", "deprecated"),
            ("urn:pending-base", "superseded"),
            ("urn:current", "accepted"),
            ("urn:proposal", "proposed"),
        ]);
        let mut successors = |iri: &str| -> anyhow::Result<Vec<String>> {
            Ok(edges
                .get(iri)
                .map(|next| next.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default())
        };
        let status = |iri: &str| statuses.get(iri).copied().unwrap_or("").to_string();
        let head =
            |start: &str, successors: &mut dyn FnMut(&str) -> anyhow::Result<Vec<String>>| {
                supersession_head(start, successors, &status).unwrap()
            };
        assert_eq!(head("urn:a", &mut successors).as_deref(), Some("urn:c"));
        assert_eq!(head("urn:loop1", &mut successors), None);
        assert_eq!(head("urn:old", &mut successors), None);
        assert_eq!(
            head("urn:pending-base", &mut successors).as_deref(),
            Some("urn:current"),
            "a proposed successor does not unseat the accepted head"
        );
    }
}
