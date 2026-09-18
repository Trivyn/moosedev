//! Read-only CodeEntity dossier queries.
//!
//! This module is deliberately a pure read path: it resolves an already-minted
//! CodeEntity, collects records directly linked to it, and returns `None` when
//! no direct knowledge exists so future hover surfaces stay quiet.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use oxigraph::model::{GraphNameRef, NamedNode, NamedNodeRef, Term};

use crate::code::substrate::symbols::normalize_symbol;
use crate::code::substrate::Position;

use super::capture::asserted_project_types;
use super::code_entities::{entities_by_symbol, file_entity_iris, CodeTerms};
use super::context::{context_item_for_iri, first_literal, render_styled_claim_body, ClaimStyle};
use super::lifecycle::in_working_set;
use super::proposals::{judgments_for_entity, JudgmentSummary};
use super::state::AppState;
use super::util::local_name;
use super::PROJECT_KG_GRAPH_IRI;

/// One dossier target: a 1-based file position, a SCIP symbol (raw or
/// normalized), or the entity IRI itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DossierTarget {
    /// Resolve the symbol at a 1-based source position through the loaded substrate.
    Position { file: String, line: u32, col: u32 },
    /// Resolve a raw or version-normalized SCIP symbol through minted entities.
    Symbol(String),
    /// Address an already-minted CodeEntity directly.
    Iri(String),
}

/// One knowledge record shown in a dossier, annotated with the canonical link
/// predicate that made it relevant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSummary {
    /// IRI of the linked knowledge record.
    pub iri: String,
    /// Local name of the record's asserted class.
    pub kind: String,
    /// Human-readable title, falling back to label/local IRI when needed.
    pub title: String,
    /// Optional captured claim describing the record.
    pub description: Option<String>,
    /// Workbench deep link when the HTTP UI has published an address.
    pub workbench_url: Option<String>,
    /// Lifecycle status; superseded records remain visible as labeled history.
    pub status: String,
    /// RFC3339 capture timestamp, or empty when the record lacks one.
    pub timestamp: String,
    /// Canonical predicate local name, regardless of which direction was asserted.
    pub predicate_local: String,
    /// Claim body the exhaustive render prints beneath the header: set only for
    /// working-set direct records, never for superseded history or component records.
    pub claim: Option<String>,
}

/// Read model for all project knowledge directly attached to a CodeEntity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dossier {
    /// IRI of the CodeEntity this dossier describes.
    pub entity_iri: String,
    /// Code-layer kind literal, such as `Function`, or `Unknown`.
    pub kind: String,
    /// Display name from code metadata, then `rdfs:label`, then IRI local name.
    pub display_name: String,
    /// Logical path from the SCIP symbol, when available.
    pub logical_path: Option<String>,
    /// Repo-relative definition path, when known.
    pub defined_in: Option<String>,
    /// Realized component IRI and label, when the entity maps to a component.
    pub realizes: Option<(String, String)>,
    /// Records directly linked to the CodeEntity; empty means no dossier is returned.
    pub direct_records: Vec<RecordSummary>,
    /// Records linked through the realized component; rendered only as secondary context.
    pub component_records: Vec<RecordSummary>,
    /// True when the loaded substrate was built from a different commit.
    pub substrate_stale: bool,
    /// True when this entity is backed by a tree-sitter syntactic identity.
    pub syntactic_anchor: bool,
    /// Role/criticality judgments (proposed = advisory, accepted = ratified).
    pub judgments: Vec<JudgmentSummary>,
    /// Observation digest lines from the churn sidecar (derived, not knowledge).
    pub observations: Vec<String>,
}

/// Return the read-only dossier for a target, or `None` when no directly linked
/// knowledge exists. Caller mistakes such as zero positions or invalid IRIs are
/// errors; absence of KG knowledge is silence.
pub fn get_entity_dossier(
    state: &AppState,
    target: &DossierTarget,
) -> anyhow::Result<Option<Dossier>> {
    let terms = CodeTerms::resolve(state)?;
    let Some(entity_iri) = resolve_target_entity(state, &terms, target)? else {
        return Ok(None);
    };

    let entity = NamedNode::new(&entity_iri)
        .with_context(|| format!("invalid CodeEntity IRI {entity_iri}"))?;
    if !asserted_project_types(state, &entity)
        .iter()
        .any(|class| class == &terms.code_entity_class)
    {
        return Ok(None);
    }

    let kind = first_literal(&state.store, &entity_iri, &terms.has_entity_kind)
        .unwrap_or_else(|| "Unknown".to_string());
    let display_name = first_literal(&state.store, &entity_iri, &terms.has_code_name)
        .or_else(|| first_literal(&state.store, &entity_iri, moose::RDFS_LABEL))
        .unwrap_or_else(|| local_name(&entity_iri).to_string());
    let logical_path = first_literal(&state.store, &entity_iri, &terms.has_logical_path);
    let defined_in = first_literal(&state.store, &entity_iri, &terms.defined_in_path);
    let syntactic_anchor = first_literal(&state.store, &entity_iri, &terms.has_substrate_symbol)
        .is_some_and(|symbol| symbol.starts_with("ts:"));
    let realizes = first_realized_component(state, &terms, &entity_iri)?;

    let mut direct_records = direct_records_for_entity(state, &entity_iri)?;
    // Silence-rule amendment (of AD 8f20452a): judgments count as direct
    // knowledge — a ratified core role with zero records is exactly the
    // hotspot a hover must surface. Observations alone never break silence
    // (measurements are not knowledge).
    let judgments = judgments_for_entity(state, &entity_iri)?;
    if direct_records.is_empty() && judgments.is_empty() {
        return Ok(None);
    }
    for record in &mut direct_records {
        if in_working_set(&record.status) {
            record.claim = record_claim(state, &record.iri, ClaimStyle::Full);
        }
    }

    let pairs = LinkPairs::resolve(state)?;
    let direct_iris = direct_records
        .iter()
        .map(|record| record.iri.as_str())
        .collect::<BTreeSet<_>>();
    let mut component_records = match realizes.as_ref() {
        Some((component_iri, _)) => collect_records(state, &pairs.concerns, component_iri)?
            .into_iter()
            .filter(|record| !direct_iris.contains(record.iri.as_str()))
            .collect(),
        None => Vec::new(),
    };
    sort_records(&mut component_records);

    // Observation digest: derived churn/authorship for the defining file.
    let mut observations = Vec::new();
    if let (Some(substrate), Some(file)) = (state.substrate(), defined_in.as_deref()) {
        if let Some(churn) = substrate.churn_for_file(file) {
            let window = substrate.churn_window_months().unwrap_or(24);
            observations.push(format!(
                "churn: {} commits/{window}mo · last {} · {} author(s) · top {:.0}%",
                churn.commits,
                churn.last_commit.get(..10).unwrap_or(&churn.last_commit),
                churn.distinct_authors,
                churn.top_author_share * 100.0
            ));
        }
    }

    Ok(Some(Dossier {
        entity_iri,
        kind,
        display_name,
        logical_path,
        defined_in,
        realizes,
        direct_records,
        component_records,
        substrate_stale: state.substrate().map(|s| s.is_stale()).unwrap_or(false),
        syntactic_anchor,
        judgments,
        observations,
    }))
}

/// Return all dossier-visible knowledge records directly linked to one CodeEntity.
pub(crate) fn direct_records_for_entity(
    state: &AppState,
    entity_iri: &str,
) -> anyhow::Result<Vec<RecordSummary>> {
    let pairs = LinkPairs::resolve(state)?;
    let mut direct_records = collect_records(state, &pairs.all, entity_iri)?;
    sort_records(&mut direct_records);
    Ok(direct_records)
}

/// Which of `code_iris` have at least one dossier-visible record linked to them.
///
/// The bulk form of [`direct_records_for_entity`], for callers that must ask the
/// question of a whole catalog at once rather than one entity at a time. It
/// shares the same predicate pairs and the same lifecycle policy, so a browse
/// surface cannot promise knowledge that hover would then decline to show —
/// the reuse `debt::why_coverage` already relies on for the same reason.
///
/// Judgments are deliberately NOT counted here, unlike [`build_dossier`]'s
/// silence-rule amendment: a ratified role is a reason for hover to speak, but
/// it is not a record any reading surface can render yet.
///
/// One predicate-bound scan per pair and direction, so the cost tracks the
/// number of links rather than the size of the catalog.
pub(crate) fn code_entities_with_records(
    state: &AppState,
    code_iris: &BTreeSet<String>,
) -> anyhow::Result<BTreeSet<String>> {
    let graph = NamedNodeRef::new_unchecked(PROJECT_KG_GRAPH_IRI);
    let pairs = LinkPairs::resolve(state)?;
    let mut linked = BTreeSet::new();
    for pair in &pairs.all {
        for (predicate_iri, entity_is_subject) in [
            (
                &pair.canonical_iri,
                matches!(pair.direction, CanonicalDirection::EntityToRecord),
            ),
            (
                &pair.inverse_iri,
                matches!(pair.direction, CanonicalDirection::RecordToEntity),
            ),
        ] {
            let predicate = NamedNodeRef::new(predicate_iri)?;
            for quad in state.store.quads_for_pattern(
                None,
                Some(predicate),
                None,
                Some(GraphNameRef::NamedNode(graph)),
            ) {
                let quad = quad?;
                let (
                    oxigraph::model::NamedOrBlankNode::NamedNode(subject),
                    Term::NamedNode(object),
                ) = (quad.subject, quad.object)
                else {
                    continue;
                };
                let (entity, record) = if entity_is_subject {
                    (subject.as_str(), object.as_str())
                } else {
                    (object.as_str(), subject.as_str())
                };
                if code_iris.contains(entity)
                    && summarize_record(state, record, pair.canonical_local).is_some()
                {
                    linked.insert(entity.to_string());
                }
            }
        }
    }
    Ok(linked)
}

/// Render one dossier's exhaustive Markdown view, as MCP `get_entity_dossier`
/// and policy push serve it.
pub fn render_markdown(dossier: &Dossier) -> String {
    render_dossiers(std::slice::from_ref(dossier))
}

/// Render the exhaustive view of one push's dossiers, in order. Working-set
/// direct records carry their claim bodies; a record an earlier section already
/// showed renders its header and points back, and a component's records are
/// listed once, at the first entity that realizes it.
pub fn render_dossiers(dossiers: &[Dossier]) -> String {
    render_dossiers_within(dossiers, None)
}

/// [`render_dossiers`] within a host byte bound. Sections render whole while the
/// running text fits `max_bytes`, and the first always renders whole. A section
/// that does not fit keeps what a bound never omits: its heading, its direct
/// records with their claims, and its component's accepted Constraint titles.
/// A closing line names the shortened entities. That core always renders, so
/// the result can exceed the bound; `None` renders every section whole.
pub(crate) fn render_dossiers_within(dossiers: &[Dossier], max_bytes: Option<usize>) -> String {
    render_dossiers_sharing(dossiers, max_bytes, &mut ShownInPush::default())
}

/// [`render_dossiers_within`] continuing a caller-owned [`ShownInPush`].
///
/// One harness prompt renders several file dossiers through separate calls;
/// sharing the state across them keeps the "claim shown above" guarantee that
/// already holds within a single call, so a record linked from two files shows
/// its claim body once per prompt instead of once per file. Every other surface
/// passes a fresh state and is byte-identical to before.
pub(crate) fn render_dossiers_sharing(
    dossiers: &[Dossier],
    max_bytes: Option<usize>,
    shown: &mut ShownInPush,
) -> String {
    let mut out = String::new();
    let mut shortened = Vec::new();
    for dossier in dossiers {
        let separator = if out.is_empty() { "" } else { "\n" };
        let mut whole_shown = shown.clone();
        let whole = render_dossier_markdown_with_records(
            dossier,
            DossierRecordRendering::Exhaustive {
                shown: &mut whole_shown,
                core_only: false,
            },
        );
        let fits = out.is_empty()
            || max_bytes.is_none_or(|max| out.len() + separator.len() + whole.len() <= max);
        out.push_str(separator);
        if fits {
            *shown = whole_shown;
            out.push_str(&whole);
        } else {
            let core = render_dossier_markdown_with_records(
                dossier,
                DossierRecordRendering::Exhaustive {
                    shown: &mut *shown,
                    core_only: true,
                },
            );
            if core != whole {
                shortened.push(format!("`{}`", dossier.display_name));
            }
            out.push_str(&core);
        }
    }
    if let (Some(max), false) = (max_bytes, shortened.is_empty()) {
        let (sections, pronoun) = if shortened.len() == 1 {
            ("section", "it")
        } else {
            ("sections", "them")
        };
        out.push_str(&format!(
            "\n{} entity {sections} shortened to direct records and accepted Constraints by the host's {max}-byte bound: {}; retrieve {pronoun} in full with get_entity_dossier\n",
            shortened.len(),
            shortened.join(", ")
        ));
    }
    out
}

/// The harness's file dossier: the exhaustive view policy push renders for every
/// knowledge-bearing entity in `file`, with harness-style claims and no
/// workbench links. `None` when nothing is linked.
///
/// A scoped exception to Constraint 2ba76439: MCP, hover and policy push keep
/// byte-identical full dossiers; only harness prompts use this rendering.
///
/// `shown` is owned by the caller and carried across every file of one prompt,
/// so a record linked from several files renders its claim once rather than
/// once per file.
pub fn harness_file_dossier(
    state: &AppState,
    file: &str,
    shown: &mut ShownInPush,
) -> anyhow::Result<Option<String>> {
    let mut dossiers = Vec::new();
    for iri in file_entity_iris(state, file)? {
        let Some(mut dossier) = get_entity_dossier(state, &DossierTarget::Iri(iri))? else {
            continue;
        };
        for record in dossier
            .direct_records
            .iter_mut()
            .chain(dossier.component_records.iter_mut())
        {
            record.workbench_url = None;
            if record.claim.is_some() {
                record.claim = record_claim(state, &record.iri, ClaimStyle::Harness);
            }
        }
        dossiers.push(dossier);
    }
    Ok((!dossiers.is_empty()).then(|| render_dossiers_sharing(&dossiers, None, shown)))
}

/// Render the editor-hover view with optional Story deep links for the exact
/// code entity and its realized component.
///
/// Hover keeps every direct record, but summarizes the potentially unbounded
/// component record list. The exhaustive MCP/policy view remains available
/// through [`render_markdown`]. The link is a parameter rather than a
/// [`Dossier`] field so liveness remains a concern of the serving surface.
pub fn render_dossier_markdown(
    dossier: &Dossier,
    entity_story_url: Option<&str>,
    component_story_url: Option<&str>,
) -> String {
    render_dossier_markdown_with_records(
        dossier,
        DossierRecordRendering::Hover {
            entity_story_url,
            component_story_url,
        },
    )
}

/// What earlier sections of one push rendered: claim and component IRIs, each
/// mapped to the display name of the entity whose section showed it.
///
/// Public so a caller rendering several dossiers in separate calls, as the
/// harness does per file, can carry one state across the whole push.
#[derive(Clone, Default)]
pub struct ShownInPush {
    claims: BTreeMap<String, String>,
    components: BTreeMap<String, String>,
    /// Bytes left for record CLAIMS in this prompt. `None` — the default every other
    /// surface takes — spends without limit and renders byte-identically to before.
    claim_budget: Option<usize>,
    /// Claims withheld once the budget ran out, counted BY KIND so the push can name
    /// what was shortened rather than only how much — a bare total leaves the model
    /// unable to tell whether a Constraint or a stale Lesson lost its prose.
    claims_withheld: BTreeMap<String, usize>,
}

impl ShownInPush {
    /// A push that spends at most `bytes` on record claims.
    ///
    /// Claims are bounded and record LINES never are, because the line is the part the
    /// study's capabilities actually rest on: it carries kind, title, **lifecycle
    /// status**, timestamp and the linking predicate, so set-completeness, negation and
    /// currency all survive the bound intact. The claim is prose — about 1,000 bytes
    /// against 150 for a line, and retrievable on demand with get_entity_dossier.
    /// Dropping records instead would shrink the inventory the harness exists to
    /// deliver, which is the one thing a bound here must never do.
    pub fn with_claim_budget(bytes: usize) -> Self {
        Self {
            claim_budget: Some(bytes),
            ..Default::default()
        }
    }

    /// How many claims the budget withheld across this prompt.
    pub fn claims_withheld(&self) -> usize {
        self.claims_withheld.values().sum()
    }

    /// The withheld claims as `Kind: n` in kind order, for the push's receipt.
    pub fn claims_withheld_by_kind(&self) -> String {
        self.claims_withheld
            .iter()
            .map(|(kind, n)| format!("{kind}: {n}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

enum DossierRecordRendering<'a> {
    /// `core_only` keeps just what a byte bound never omits: the heading,
    /// direct records with their claims, and accepted Constraint titles.
    Exhaustive {
        shown: &'a mut ShownInPush,
        core_only: bool,
    },
    Hover {
        entity_story_url: Option<&'a str>,
        component_story_url: Option<&'a str>,
    },
}

fn render_dossier_markdown_with_records(
    dossier: &Dossier,
    mut record_rendering: DossierRecordRendering<'_>,
) -> String {
    let core_only = matches!(
        record_rendering,
        DossierRecordRendering::Exhaustive {
            core_only: true,
            ..
        }
    );
    let marker = if dossier.syntactic_anchor {
        " [syntactic anchor]"
    } else {
        ""
    };
    let mut out = format!(
        "### {} ({}){}\n",
        dossier.display_name, dossier.kind, marker
    );
    if dossier.logical_path.is_some() || dossier.defined_in.is_some() {
        match (&dossier.logical_path, &dossier.defined_in) {
            (Some(path), Some(defined)) => {
                out.push_str(&format!("`{path}` - defined in `{defined}`\n"));
            }
            (Some(path), None) => out.push_str(&format!("`{path}`\n")),
            (None, Some(defined)) => out.push_str(&format!("Defined in `{defined}`\n")),
            (None, None) => {}
        }
    }
    if let Some((_, label)) = &dossier.realizes {
        out.push_str(&format!("Realizes component: {label}\n"));
    }
    if let DossierRecordRendering::Hover {
        entity_story_url: Some(url),
        ..
    } = &record_rendering
    {
        out.push_str(&format!("\n[Tell me the Story]({url})\n"));
    }

    if !dossier.judgments.is_empty() && !core_only {
        out.push_str("\n**Judgments**\n");
        for judgment in &dossier.judgments {
            render_judgment_line(&mut out, judgment);
        }
        // §5.3 item 5: a ratified core/critical entity with no linked
        // rationale is a comprehension-debt hotspot with an address.
        let core_or_critical = dossier.judgments.iter().any(|j| {
            j.status == "accepted"
                && matches!(
                    j.target_local.as_str(),
                    "core-algorithm" | "domain-logic" | "high"
                )
        });
        if core_or_critical && dossier.direct_records.is_empty() {
            out.push_str("⚠ core entity — no linked rationale\n");
        }
    }

    if !dossier.direct_records.is_empty() {
        match &mut record_rendering {
            DossierRecordRendering::Exhaustive { shown, .. } => {
                out.push_str("\n**Records**\n");
                for record in &dossier.direct_records {
                    render_record_line(&mut out, record);
                    render_record_claim(&mut out, record, &dossier.display_name, shown);
                }
            }
            DossierRecordRendering::Hover { .. } => {
                out.push_str("\n**Direct records**\n");
                for record in &dossier.direct_records {
                    render_record_line(&mut out, record);
                }
            }
        }
    }
    if let Some((component_iri, label)) = &dossier.realizes {
        if !dossier.component_records.is_empty() {
            match &mut record_rendering {
                DossierRecordRendering::Exhaustive { shown, .. } => {
                    if let Some(first) = shown.components.get(component_iri) {
                        out.push_str(&format!(
                            "\n**Via component {label}**: listed above for `{first}`\n"
                        ));
                    } else {
                        shown
                            .components
                            .insert(component_iri.clone(), dossier.display_name.clone());
                        out.push_str(&format!("\n**Via component {label}**\n"));
                        let limit = if core_only { 0 } else { COMPONENT_TITLE_LIMIT };
                        render_component_record_titles(
                            &mut out,
                            label,
                            &dossier.component_records,
                            limit,
                        );
                    }
                }
                DossierRecordRendering::Hover {
                    component_story_url,
                    ..
                } => {
                    render_component_record_summary(
                        &mut out,
                        &dossier.component_records,
                        *component_story_url,
                    );
                }
            }
        }
    }
    if !dossier.observations.is_empty() && !core_only {
        out.push_str("\n**Observations**\n");
        for observation in &dossier.observations {
            out.push_str(&format!("- {observation}\n"));
        }
    }
    if dossier.substrate_stale {
        out.push_str(
            "\nwarning: substrate is stale; positions may have drifted, re-run `moosedev index`.\n",
        );
    }
    out
}

/// A working-set direct record's claim body, unindented beneath its header. A
/// claim an earlier section of the same push showed points back instead.
fn render_record_claim(
    out: &mut String,
    record: &RecordSummary,
    entity_name: &str,
    shown: &mut ShownInPush,
) {
    let Some(claim) = &record.claim else {
        return;
    };
    if let Some(first) = shown.claims.get(&record.iri) {
        out.push_str(&format!("claim shown above for `{first}`\n"));
        return;
    }
    // A claim budget — set only by the harness, per AD 21855a2a's allowance for a byte
    // bound with an explicit notice — is spent here and never on record lines. The fit is
    // greedy rather than a hard stop: one oversized claim is skipped while the remaining
    // budget still buys the smaller claims after it, which delivers strictly more
    // knowledge than stopping at the first claim that does not fit.
    if let Some(remaining) = shown.claim_budget.as_mut() {
        if claim.len() > *remaining {
            *shown
                .claims_withheld
                .entry(record.kind.clone())
                .or_default() += 1;
            return;
        }
        *remaining -= claim.len();
    }
    out.push_str(claim);
    // Only a claim actually rendered may be pointed at later; recording a withheld one
    // would leave a "claim shown above" pointing at text no prompt ever contained.
    shown
        .claims
        .insert(record.iri.clone(), entity_name.to_string());
}

/// Component records listed by title before the omission line; accepted
/// Constraints are listed on top of this.
const COMPONENT_TITLE_LIMIT: usize = 12;

/// Exhaustive component context: title lines only. Accepted Constraints are
/// always listed, other records up to `limit` in dossier order, then one line
/// counts the rest by kind.
fn render_component_record_titles(
    out: &mut String,
    label: &str,
    records: &[RecordSummary],
    limit: usize,
) {
    let mut listed = 0;
    let mut omitted = Vec::new();
    for record in records {
        let accepted_constraint =
            record.kind == "Constraint" && record.status.eq_ignore_ascii_case("accepted");
        if accepted_constraint || listed < limit {
            listed += usize::from(!accepted_constraint);
            render_record_line(out, record);
        } else {
            omitted.push(record);
        }
    }
    if !omitted.is_empty() {
        let (noun, verb) = record_noun_verb(omitted.len());
        out.push_str(&format!(
            "{} further {noun} {verb} component {label} ({}), not necessarily this code; search project knowledge for records about {label}\n",
            omitted.len(),
            kind_counts(omitted.iter().copied())
        ));
    }
}

/// `Kind: n; Kind: n` over records, kinds in name order.
fn kind_counts<'a>(records: impl IntoIterator<Item = &'a RecordSummary>) -> String {
    let mut by_kind = BTreeMap::<&str, usize>::new();
    for record in records {
        *by_kind.entry(&record.kind).or_default() += 1;
    }
    by_kind
        .into_iter()
        .map(|(kind, count)| format!("{kind}: {count}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn record_noun_verb(count: usize) -> (&'static str, &'static str) {
    if count == 1 {
        ("record", "concerns")
    } else {
        ("records", "concern")
    }
}

fn render_component_record_summary(
    out: &mut String,
    records: &[RecordSummary],
    component_story_url: Option<&str>,
) {
    let counts = kind_counts(records);
    let (noun, verb) = record_noun_verb(records.len());

    out.push_str("\n**Indirect component context**\n");
    out.push_str(&format!(
        "- Total: {} {noun} that {verb} this component, not necessarily this code entity.\n",
        records.len()
    ));
    out.push_str(&format!("- By kind: {counts}\n"));
    if let Some(url) = component_story_url {
        out.push_str(&format!("- [Tell me the component Story]({url})\n"));
    }
}

/// One judgment line: ratified plain with provenance, proposed visually
/// distinct with confidence + disposition (spec §5.3: provisional judgments
/// render as proposals).
fn render_judgment_line(out: &mut String, judgment: &JudgmentSummary) {
    let axis = match judgment.predicate_local.as_str() {
        "playsRole" => "role",
        "hasCriticality" => "criticality",
        other => other,
    };
    if judgment.status == "accepted" {
        let date = judgment.timestamp.get(..10).unwrap_or(&judgment.timestamp);
        out.push_str(&format!(
            "- {axis}: {} — proposed by {}, ratified {date}\n",
            judgment.target_local, judgment.author
        ));
    } else {
        let confidence = judgment.confidence.as_deref().unwrap_or("?");
        let escalation = judgment.escalation.as_deref().unwrap_or("pending");
        out.push_str(&format!(
            "- {axis}: {}? (proposed, {confidence}, {escalation})\n",
            judgment.target_local
        ));
    }
}

fn render_record_line(out: &mut String, record: &RecordSummary) {
    let title = match &record.workbench_url {
        Some(url) => format!("[{}]({url})", record.title),
        None => record.title.clone(),
    };
    out.push_str(&format!(
        "- [{}] {} - {}, {} (via {})\n",
        record.kind, title, record.status, record.timestamp, record.predicate_local
    ));
}

/// Convert the caller's selector into an existing CodeEntity IRI without minting.
/// Crate-visible so the policy engine can resolve gate candidates without the
/// dossier's "no linked records → silence" behavior.
pub(crate) fn resolve_target_entity(
    state: &AppState,
    terms: &CodeTerms,
    target: &DossierTarget,
) -> anyhow::Result<Option<String>> {
    match target {
        DossierTarget::Position { file, line, col } => {
            if *line == 0 || *col == 0 {
                anyhow::bail!("code positions are 1-based; line and col must be greater than 0");
            }
            let Some(substrate) = state.substrate() else {
                return Ok(None);
            };
            let Some(resolution) = substrate.resolve(
                file,
                Position {
                    line: line - 1,
                    col: col - 1,
                },
            ) else {
                return Ok(None);
            };
            if resolution.is_local {
                return Ok(None);
            }
            let Some(symbol) = normalize_symbol(&resolution.symbol) else {
                return Ok(None);
            };
            Ok(entities_by_symbol(state, terms)?.get(&symbol).cloned())
        }
        DossierTarget::Symbol(symbol) => {
            let normalized = normalize_symbol(symbol).unwrap_or_else(|| symbol.clone());
            Ok(entities_by_symbol(state, terms)?.get(&normalized).cloned())
        }
        DossierTarget::Iri(iri) => {
            NamedNode::new(iri).with_context(|| format!("invalid entity IRI {iri}"))?;
            Ok(Some(iri.clone()))
        }
    }
}

/// Every component the entity `realizes`, in IRI order.
pub(super) fn realized_components(
    state: &AppState,
    terms: &CodeTerms,
    entity_iri: &str,
) -> anyhow::Result<Vec<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(entity_iri)?;
    let predicate = NamedNodeRef::new(&terms.realizes)?;
    let mut out = BTreeSet::new();
    for q in state.store.quads_for_pattern(
        Some(subject.into()),
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        if let Term::NamedNode(component) = q?.object {
            out.insert(component.as_str().to_string());
        }
    }
    Ok(out.into_iter().collect())
}

/// Return the first component reached by `realizes`, with a display label.
fn first_realized_component(
    state: &AppState,
    terms: &CodeTerms,
    entity_iri: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let subject = NamedNodeRef::new(entity_iri)?;
    let predicate = NamedNodeRef::new(&terms.realizes)?;
    for q in state.store.quads_for_pattern(
        Some(subject.into()),
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        let Term::NamedNode(component) = q.object else {
            continue;
        };
        let iri = component.as_str().to_string();
        let label = first_literal(&state.store, &iri, moose::RDFS_LABEL)
            .unwrap_or_else(|| local_name(&iri).to_string());
        return Ok(Some((iri, label)));
    }
    Ok(None)
}

/// Canonical predicate plus inverse predicate used to find records regardless of
/// which direction was asserted in the project graph.
#[derive(Debug, Clone)]
pub(super) struct PredicatePair {
    canonical_local: &'static str,
    canonical_iri: String,
    inverse_iri: String,
    direction: CanonicalDirection,
}

impl PredicatePair {
    /// Resolve one canonical/inverse object-property pair by local name.
    pub(super) fn new(
        state: &AppState,
        canonical_local: &'static str,
        inverse_local: &str,
        direction: CanonicalDirection,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            canonical_local,
            canonical_iri: state.resolve_object_property(canonical_local)?,
            inverse_iri: state.resolve_object_property(inverse_local)?,
            direction,
        })
    }
}

/// Orientation of the canonical predicate relative to the target: whether the
/// canonical edge points at the target (`RecordToEntity`) or away from it.
#[derive(Debug, Clone, Copy)]
pub(super) enum CanonicalDirection {
    RecordToEntity,
    EntityToRecord,
}

/// Resolved predicate sets for direct records and component secondary records.
pub(super) struct LinkPairs {
    pub(super) concerns: Vec<PredicatePair>,
    /// `concerns` and `constrains`: the edges by which a record governs a component.
    pub(super) component: Vec<PredicatePair>,
    pub(super) all: Vec<PredicatePair>,
}

impl LinkPairs {
    /// Resolve the object properties by local name, keeping ontology namespaces
    /// out of the read logic.
    pub(super) fn resolve(state: &AppState) -> anyhow::Result<Self> {
        let specs = [
            (
                "concerns",
                "isConcernedBy",
                CanonicalDirection::RecordToEntity,
            ),
            (
                "constrains",
                "isConstrainedBy",
                CanonicalDirection::RecordToEntity,
            ),
            (
                "satisfies",
                "isSatisfiedBy",
                CanonicalDirection::EntityToRecord,
            ),
            (
                "embodies",
                "isEmbodiedBy",
                CanonicalDirection::EntityToRecord,
            ),
            (
                "violates",
                "isViolatedBy",
                CanonicalDirection::EntityToRecord,
            ),
        ];
        let all = specs
            .into_iter()
            .map(|(canonical_local, inverse_local, direction)| {
                PredicatePair::new(state, canonical_local, inverse_local, direction)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            concerns: all[..1].to_vec(),
            component: all[..2].to_vec(),
            all,
        })
    }
}

/// Collect summaries for all records linked to one target by any supplied pair.
pub(super) fn collect_records(
    state: &AppState,
    pairs: &[PredicatePair],
    target_iri: &str,
) -> anyhow::Result<Vec<RecordSummary>> {
    let mut records = BTreeMap::new();
    for pair in pairs {
        for record_iri in linked_records(state, pair, target_iri)? {
            if let Some(summary) = summarize_record(state, &record_iri, pair.canonical_local) {
                records.insert((record_iri, pair.canonical_local), summary);
            }
        }
    }
    Ok(records.into_values().collect())
}

/// Return candidate record IRIs linked through the canonical or inverse edge.
pub(super) fn linked_records(
    state: &AppState,
    pair: &PredicatePair,
    target_iri: &str,
) -> anyhow::Result<BTreeSet<String>> {
    let graph = NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?;
    let target = NamedNodeRef::new(target_iri)?;
    let mut out = BTreeSet::new();
    match pair.direction {
        CanonicalDirection::RecordToEntity => {
            collect_subjects(state, &pair.canonical_iri, target, graph, &mut out)?;
            collect_objects(state, target, &pair.inverse_iri, graph, &mut out)?;
        }
        CanonicalDirection::EntityToRecord => {
            collect_objects(state, target, &pair.canonical_iri, graph, &mut out)?;
            collect_subjects(state, &pair.inverse_iri, target, graph, &mut out)?;
        }
    }
    Ok(out)
}

/// Add named subjects from `(?subject, predicate, object)` matches.
fn collect_subjects(
    state: &AppState,
    predicate_iri: &str,
    object: NamedNodeRef<'_>,
    graph: NamedNodeRef<'_>,
    out: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    let predicate = NamedNodeRef::new(predicate_iri)?;
    for q in state.store.quads_for_pattern(
        None,
        Some(predicate),
        Some(object.into()),
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        if let oxigraph::model::NamedOrBlankNode::NamedNode(subject) = q.subject {
            out.insert(subject.as_str().to_string());
        }
    }
    Ok(())
}

/// Add named objects from `(subject, predicate, ?object)` matches.
fn collect_objects(
    state: &AppState,
    subject: NamedNodeRef<'_>,
    predicate_iri: &str,
    graph: NamedNodeRef<'_>,
    out: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    let predicate = NamedNodeRef::new(predicate_iri)?;
    for q in state.store.quads_for_pattern(
        Some(subject.into()),
        Some(predicate),
        None,
        Some(GraphNameRef::NamedNode(graph)),
    ) {
        let q = q?;
        if let Term::NamedNode(object) = q.object {
            out.insert(object.as_str().to_string());
        }
    }
    Ok(())
}

/// Build a display summary for a record, skipping dangling and dossier-hidden nodes.
pub(super) fn summarize_record(
    state: &AppState,
    record_iri: &str,
    predicate_local: &str,
) -> Option<RecordSummary> {
    let subject = NamedNode::new(record_iri).ok()?;
    let kind_iri = asserted_project_types(state, &subject).into_iter().next()?;
    let title = first_literal(&state.store, record_iri, &state.capture.title)
        .or_else(|| first_literal(&state.store, record_iri, moose::RDFS_LABEL))
        .unwrap_or_else(|| local_name(record_iri).to_string());
    let status = first_literal(&state.store, record_iri, &state.capture.status)
        .unwrap_or_else(|| "unknown".to_string());
    // Proposed records live only in the ratification inbox; rejected and
    // deprecated records are hidden history. Superseded records remain visible
    // as labeled history so a dossier preserves the rationale's evolution.
    if !is_dossier_visible(&status) {
        return None;
    }
    Some(RecordSummary {
        iri: record_iri.to_string(),
        kind: local_name(&kind_iri).to_string(),
        title,
        description: first_literal(&state.store, record_iri, &state.capture.description),
        workbench_url: workbench_record_url(state, record_iri, local_name(&kind_iri)),
        status,
        timestamp: first_literal(&state.store, record_iri, &state.capture.timestamp)
            .unwrap_or_default(),
        predicate_local: predicate_local.to_string(),
        claim: None,
    })
}

/// The claim body topic recall renders for a record in `style`, or `None` when
/// the record states nothing beyond its title and stamps.
fn record_claim(state: &AppState, record_iri: &str, style: ClaimStyle) -> Option<String> {
    let item = context_item_for_iri(state, record_iri, false)?;
    let mut claim = String::new();
    render_styled_claim_body(state, &item, style, &mut claim);
    (!claim.is_empty()).then_some(claim)
}

/// Dossier lifecycle policy shared by direct records, inherited component
/// records, hover rendering, and why-coverage through [`summarize_record`].
fn is_dossier_visible(status: &str) -> bool {
    in_working_set(status) || status.eq_ignore_ascii_case("superseded")
}

/// Return a fresh workbench deep link for a record when the HTTP UI is
/// serving in THIS daemon run.
///
/// The in-process address is the only trusted source: the `http.addr` file
/// lingers after a crash, so after an HTTP-disabled or failed restart it can
/// point at a dead port or an unrelated local service. (The file stays for
/// cross-process `--status`/`ui` discovery.)
pub(crate) fn workbench_record_url(
    state: &AppState,
    record_iri: &str,
    kind: &str,
) -> Option<String> {
    let addr = state.http_addr()?;
    let route = match kind {
        "ArchitecturalDecision" => "adrs",
        "Requirement" => "requirements",
        "Lesson" => "lessons",
        "Constraint" => "constraints",
        _ => "record",
    };
    Some(format!(
        "http://{addr}/#/{route}/{}",
        encode_path_segment(addressable_local_name(record_iri)?)
    ))
}

/// Workbench deep link for a code entity. The `#/record/<uuid>` route resolves
/// any typed project-graph subject by UUID, CodeEntities included, so entity
/// links need no dedicated route.
pub(crate) fn workbench_entity_url(state: &AppState, entity_iri: &str) -> Option<String> {
    workbench_record_url(state, entity_iri, "CodeEntity")
}

/// Workbench deep link that tells the Story of one graph entity. Same liveness rule as
/// [`workbench_record_url`]: absent whenever this daemon run is not serving
/// HTTP, so a hover never offers a dead port.
pub(crate) fn workbench_story_url(state: &AppState, subject_iri: &str) -> Option<String> {
    let addr = state.http_addr()?;
    Some(format!(
        "http://{addr}/#/stories/entity/{}",
        encode_path_segment(addressable_local_name(subject_iri)?)
    ))
}

/// An IRI's final segment, but only when a workbench route can actually resolve
/// it. `record_iri_for_uuid` matches subjects ending in `/{uuid}`, so a
/// fragment-addressed IRI like `https://example.test/records#decision` — which
/// `local_name` happily reduces to `decision` — would advertise a link
/// resolving to nothing, or to an unrelated record ending in `/decision`.
/// Silence beats a link that lies.
fn addressable_local_name(iri: &str) -> Option<&str> {
    let slash = iri.rfind('/')?;
    if iri.rfind('#').is_some_and(|hash| hash > slash) {
        return None;
    }
    let name = local_name(iri);
    (!name.is_empty()).then_some(name)
}

/// Percent-encode one URL path segment. Minted local names are UUIDs, for
/// which this is a no-op; it exists so a hand-authored or legacy IRI cannot
/// inject path or query structure into a link a user is about to open.
fn encode_path_segment(segment: &str) -> String {
    segment
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Keep dossier output stable and useful: constraints first, then decisions,
/// then lessons, with newer records before older records inside each group.
pub(super) fn sort_records(records: &mut [RecordSummary]) {
    records.sort_by(|a, b| {
        kind_rank(&a.kind)
            .cmp(&kind_rank(&b.kind))
            .then_with(|| b.timestamp.cmp(&a.timestamp))
            .then_with(|| a.iri.cmp(&b.iri))
    });
}

/// Rank the record classes that are most important in hover-sized context.
fn kind_rank(kind: &str) -> u8 {
    match kind {
        "Constraint" => 0,
        "ArchitecturalDecision" => 1,
        "Lesson" => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::encode_path_segment;

    #[test]
    fn path_segments_are_percent_encoded() {
        // The ordinary case: a minted UUID passes through untouched, so
        // existing workbench links are byte-identical.
        let uuid = "74af589c-b13f-4224-99fd-4f85641daea7";
        assert_eq!(encode_path_segment(uuid), uuid);

        // A hand-authored or legacy local name cannot inject path or query
        // structure into a link a user is about to open in a browser.
        assert_eq!(encode_path_segment("a/b"), "a%2Fb");
        assert_eq!(encode_path_segment("a?b#c"), "a%3Fb%23c");
        assert_eq!(encode_path_segment("a b"), "a%20b");
        assert_eq!(encode_path_segment("naïve"), "na%C3%AFve");
        // Unreserved characters stay readable.
        assert_eq!(encode_path_segment("A-z_0.9~"), "A-z_0.9~");
    }
}
