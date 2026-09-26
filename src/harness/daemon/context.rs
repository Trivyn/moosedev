//! Context for the runner: the governing knowledge of the files it reads,
//! the accepted-revision fingerprint every later operation is pinned to, and
//! the current record inventory the link path chooses from (listed in the
//! context only while the walk supplies no rules or linked evidence).
use super::*;
use crate::harness::digest::sha256_hex;

pub async fn context(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ContextRequest>,
) -> Result<Json<ContextResponse>, ApiError> {
    Ok(Json(context_snapshot(&state, &request)?))
}

/// Bytes one prompt may spend on record claims, tunable so the floor study can sweep it.
/// The default leaves room for the task, plan and history in a 64k window once record
/// lines (the inventory, never bounded) are paid for.
fn claim_budget() -> usize {
    std::env::var("MOOSEDEV_HARNESS_CLAIM_BUDGET")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(12_000)
}

pub fn context_snapshot(
    state: &AppState,
    request: &ContextRequest,
) -> anyhow::Result<ContextResponse> {
    let generation = state.project_write_generation();
    anyhow::ensure!(!request.topic.trim().is_empty(), "context topic is empty");
    anyhow::ensure!(
        request.files.len() <= 100,
        "at most 100 files per context request"
    );
    anyhow::ensure!(
        request.rule_files.len() <= 100,
        "at most 100 rule files per context request"
    );
    anyhow::ensure!(
        !request.evidence_only || (request.files.is_empty() && request.rule_files.is_empty()),
        "an evidence-only context request takes no files"
    );
    state.try_ensure_enriched()?;
    let mut context = String::new();
    let mut governing_rules = Vec::new();
    let mut context_records = Vec::new();
    let mut record_indexes = std::collections::BTreeMap::new();
    let mut file_record_iris = Vec::new();
    let mut delivery_receipt = None;
    let evidence_iris = if request.evidence_only {
        // An evidence-only request (the model's search) returns only atomic
        // record blocks. When the caller supplies a byte budget the daemon,
        // which owns retrieval policy, degrades whole records and accounts for
        // every selected record instead of leaving the runner to cut arbitrary
        // bytes through the middle of a claim.
        let records = graph::relevant_context_snapshot(state, Some(&request.topic), 12, false)?;
        for record in &records {
            merge_context_record(
                &mut context_records,
                &mut record_indexes,
                context_record(state, record, true, "topic match"),
            );
        }
        let rendered = render_topic_records_within(state, &records, request.max_bytes)?;
        context = rendered.context;
        delivery_receipt = Some(rendered.receipt);
        rendered.evidence_iris
    } else {
        // Linked evidence leads (AD 85da8700): the walk from the files' code
        // replaces similarity-ranked topic recall, which remains only as a
        // fallback when nothing is linked beyond what the dossiers print.
        // An approved spec in the working set is about the components its
        // records govern, wherever those live: reading `crate.md` at the
        // project root delivers the rules of `crate/` while planning, before
        // any file under it exists.
        let mut walked = request.files.clone();
        walked.extend(
            request
                .rule_files
                .iter()
                .filter(|file| !request.files.contains(file))
                .cloned(),
        );
        let governed = super::spec::spec_components_for_files(state, &walked)?;
        let linked = graph::linked_evidence(state, &walked, &governed)?;
        file_record_iris.extend(linked.excluded.iter().cloned());
        governing_rules = graph::governing_rules(&linked)
            .into_iter()
            .map(|rule| GoverningRule {
                via: rule.hop.via(&rule.source),
                iri: rule.iri,
                label: rule.label,
                kind: rule.kind,
                claim: rule.claim,
            })
            .collect();
        for record in &linked.records {
            merge_context_record(
                &mut context_records,
                &mut record_indexes,
                ContextRecord {
                    iri: record.iri.clone(),
                    kind: record.kind.clone(),
                    title: record.label.clone(),
                    claim: record.claim.clone(),
                    provenance: vec![record.hop.via(&record.source)],
                },
            );
        }
        for rule in &governing_rules {
            merge_context_record(
                &mut context_records,
                &mut record_indexes,
                ContextRecord {
                    iri: rule.iri.clone(),
                    kind: rule.kind.clone(),
                    title: rule.label.clone(),
                    claim: rule.claim.clone(),
                    provenance: vec![rule.via.clone()],
                },
            );
        }
        // The record-name inventory stays only while the walk supplies no
        // rules and no linked evidence, as on the first request with no files.
        if linked.records.is_empty() && governing_rules.is_empty() {
            context.push_str(&format!("Recall: the inventory lists current record names only; {RECALL}\n\nCurrent knowledge inventory:\n"));
            for record in graph::relevant_context_snapshot(state, None, 100, false)? {
                context.push_str(&format!(
                    "[{}] {} ({})\n",
                    record.kind, record.label, record.iri
                ));
                merge_context_record(
                    &mut context_records,
                    &mut record_indexes,
                    context_record(state, &record, false, "current inventory"),
                );
            }
        } else {
            context.push_str(&format!("Recall: {RECALL}\n"));
        }
        if linked.records.is_empty() {
            let fallback: Vec<_> =
                graph::relevant_context_snapshot(state, Some(&request.topic), 5, false)?
                    .into_iter()
                    .filter(|record| !linked.excluded.contains(&record.iri))
                    .collect();
            context.push_str("\nTopic evidence (fallback; nothing is linked beyond the file dossiers; complete claims; up to six relationships per record):\n");
            render_topic_records(state, &mut context, &fallback);
            for record in &fallback {
                merge_context_record(
                    &mut context_records,
                    &mut record_indexes,
                    context_record(state, record, true, "topic fallback"),
                );
            }
        } else {
            context.push_str("\nLinked evidence (records linked to the files' code and components; complete claims):\n");
            // A governing rule is listed once, under Project rules, with its
            // via: line and claim. Repeating it here as a pointer cost about
            // 180 bytes per rule and told the model nothing (86 rules, 15.6 KB
            // in badciv 7e0c50eb).
            let (rules, evidence): (Vec<_>, Vec<_>) = linked
                .records
                .iter()
                .cloned()
                .partition(|record| graph::is_rule_kind(&record.kind));
            context.push_str(&graph::render_linked_evidence(&evidence));
            if !rules.is_empty() {
                context.push_str(&format!(
                    "\n{} governing rule(s) linked here are listed with their claims under Project rules.\n",
                    rules.len()
                ));
            }
        }
        Vec::new()
    };
    let root = state.project_root();
    let mut files = Vec::new();
    // One dedup state for the whole prompt, not one per file: these dossiers are
    // rendered separately but reach the model together, so a record linked from
    // several files should show its claim body once, as it already does within a
    // single file's render.
    // …and one claim budget for the whole prompt (task #43). The file dossiers were the
    // only unbounded section of the push: file_entity_iris returns EVERY definition in a
    // file, direct_records is capped nowhere, and this path alone passed no byte bound —
    // one file measured 86,693 bytes. Because model.rs counts dossiers as MANDATORY, that
    // growth evicted history, observations and navigation before overflowing the window,
    // which is the study's dominant harness-specific failure (Lesson af16b95e).
    //
    // The budget binds CLAIMS only, never record lines. The line carries kind, title,
    // lifecycle status, timestamp and linking predicate, so set-completeness, negation and
    // currency all survive it intact; the claim is prose, roughly 1,000 bytes against 150,
    // and retrievable on demand. Bounding the inventory instead would shrink exactly what
    // the harness exists to deliver. AD 21855a2a allows a byte bound with an explicit
    // notice, which the response carries below; the bound is computed here in the daemon,
    // so no surface grows its own policy (Constraint 2ba76439).
    let mut shown = graph::ShownInPush::with_claim_budget(claim_budget());
    for file in &request.files {
        validate_path(file)?;
        // Records are never dropped; only their claims are bounded, and the push says so.
        let dossier = graph::harness_file_dossier(state, file, &mut shown)?.unwrap_or_else(|| {
            "No recorded entity knowledge is linked to this file. Topic recall still applies."
                .into()
        });
        let policy = policy::evaluate(
            state,
            &root,
            &PolicyEvent::EditProposed {
                file: file.clone(),
                line: None,
                col: None,
                anchor: None,
            },
        )?;
        files.push(FileContext {
            file: file.clone(),
            dossier,
            policy,
        });
    }
    if !request.evidence_only {
        for iri in &file_record_iris {
            if let Some(record) = graph::context_item_for_iri(state, iri, false) {
                merge_context_record(
                    &mut context_records,
                    &mut record_indexes,
                    context_record(state, &record, true, "file dossier"),
                );
            }
        }
    }
    // The explicit notice AD 21855a2a requires. Without it a shorter dossier reads as a
    // smaller graph — the same misreading that had two models report an empty graph.
    if shown.claims_withheld() > 0 {
        context.push_str(&format!(
            "\n\n{} record claim(s) withheld by the {}-byte push bound ({}). Every record above \
             is still listed with its kind, title and lifecycle status; retrieve any claim in full \
             with get_entity_dossier.\n",
            shown.claims_withheld(),
            claim_budget(),
            shown.claims_withheld_by_kind()
        ));
    }
    // The model-facing push keeps its established claim bounds. The Knowledge
    // view is an audit surface, so every selected non-inventory record gets its
    // complete claim even when the prompt rendered only a pointer.
    for record in &mut context_records {
        if record
            .provenance
            .iter()
            .any(|source| source != "current inventory")
        {
            if let Some(item) = graph::context_item_for_iri(state, &record.iri, false) {
                record.claim.clear();
                graph::render_styled_claim_body(
                    state,
                    &item,
                    graph::ClaimStyle::Full,
                    &mut record.claim,
                );
                record.claim = record.claim.trim().to_owned();
            }
        }
    }
    // An approved spec whose file moved on governs with possibly stale
    // records; say so where the model and the human both read.
    let approved_specs = if request.evidence_only {
        Vec::new()
    } else {
        super::spec::approved_spec_statuses(state)?
    };
    for spec in approved_specs.iter().filter(|spec| spec.stale) {
        context.push_str(&format!(
            "\nApproved spec {} changed since its approval ({} record(s) may be stale); run /approve-spec {} to reconcile them.\n",
            spec.path, spec.record_count, spec.path
        ));
    }
    // What an approved spec still asks for, so a later task does not take an
    // earlier task's completion for the spec's.
    for spec in approved_specs.iter().filter(|spec| {
        spec.open_rules
            .as_ref()
            .is_some_and(|open| !open.is_empty())
    }) {
        if let Some(line) = spec.progress() {
            context.push_str(&format!("\n{line}\n"));
        }
    }
    let revision = accepted_revision(state)?;
    anyhow::ensure!(
        generation == state.project_write_generation(),
        "project knowledge changed while assembling context; retry retrieval"
    );
    Ok(ContextResponse {
        approved_specs,
        project_root: root.to_string_lossy().into_owned(),
        revision,
        context,
        files,
        records: context_records,
        evidence_iris,
        delivery_receipt,
        capture_contracts: vec![2, 3],
        intent_contracts: vec![2],
        context_contracts: vec![1],
        governing_rules,
    })
}

fn context_record(
    state: &AppState,
    record: &graph::ContextItem,
    include_claim: bool,
    provenance: &str,
) -> ContextRecord {
    let mut claim = String::new();
    if include_claim {
        graph::render_styled_claim_body(state, record, graph::ClaimStyle::Full, &mut claim);
    }
    ContextRecord {
        iri: record.iri.clone(),
        kind: record.kind.clone(),
        title: record.label.clone(),
        claim: claim.trim().to_owned(),
        provenance: vec![provenance.to_owned()],
    }
}

fn merge_context_record(
    records: &mut Vec<ContextRecord>,
    indexes: &mut std::collections::BTreeMap<String, usize>,
    incoming: ContextRecord,
) {
    if let Some(index) = indexes.get(&incoming.iri).copied() {
        let current = &mut records[index];
        if current.claim.is_empty() && !incoming.claim.is_empty() {
            current.claim = incoming.claim;
        }
        for source in incoming.provenance {
            if !current.provenance.contains(&source) {
                current.provenance.push(source);
            }
        }
        return;
    }
    indexes.insert(incoming.iri.clone(), records.len());
    records.push(incoming);
}

/// How recall reaches claims, after the inventory sentence when it is listed.
const RECALL: &str = "search with words from a record name returns its complete claims. A structural walk from the attached files' code and components supplies the linked evidence; topic recall appears only when nothing is linked beyond the file dossiers. Attached file dossiers carry the complete claims of records linked to the file's code, and governing Constraints appear with their claims under Project rules.";

/// Topic recall records: a header per record, then its harness-style claim body.
fn render_topic_records(state: &AppState, context: &mut String, records: &[graph::ContextItem]) {
    for record in records {
        context.push_str(&topic_record_header(record));
        context.push_str(&topic_record_claim(state, record));
    }
}

struct TopicDelivery<'a> {
    record: &'a graph::ContextItem,
    full_block: String,
    first_sentence_block: String,
    title_block: String,
    tier: ContextRecordDeliveryTier,
}

impl<'a> TopicDelivery<'a> {
    fn new(state: &AppState, record: &'a graph::ContextItem) -> Self {
        const SHORTENED: &str =
            "claim shortened by the caller byte bound; search this record title for the complete claim\n";
        const WITHHELD: &str =
            "claim withheld by the caller byte bound; search this record title for the complete claim\n";

        let header = topic_record_header(record);
        let claim = topic_record_claim(state, record);
        let full_block = format!("{header}{claim}");
        let mut first_sentence_block = format!("{header}{}", first_claim_sentence(&claim));
        if !first_sentence_block.ends_with('\n') {
            first_sentence_block.push('\n');
        }
        first_sentence_block.push_str(SHORTENED);
        let title_block = format!("{header}{WITHHELD}");
        Self {
            record,
            full_block,
            first_sentence_block,
            title_block,
            tier: ContextRecordDeliveryTier::FullClaim,
        }
    }

    fn block(&self) -> &str {
        match self.tier {
            ContextRecordDeliveryTier::FullClaim => &self.full_block,
            ContextRecordDeliveryTier::FirstSentence => &self.first_sentence_block,
            ContextRecordDeliveryTier::TitleOnly => &self.title_block,
            ContextRecordDeliveryTier::Omitted => "",
        }
    }

    fn next_tier(&self) -> Option<ContextRecordDeliveryTier> {
        match self.tier {
            ContextRecordDeliveryTier::FullClaim => Some(ContextRecordDeliveryTier::FirstSentence),
            ContextRecordDeliveryTier::FirstSentence => Some(ContextRecordDeliveryTier::TitleOnly),
            ContextRecordDeliveryTier::TitleOnly if self.record.kind != "Constraint" => {
                Some(ContextRecordDeliveryTier::Omitted)
            }
            ContextRecordDeliveryTier::TitleOnly | ContextRecordDeliveryTier::Omitted => None,
        }
    }
}

struct RenderedTopicEvidence {
    context: String,
    evidence_iris: Vec<String>,
    receipt: ContextDeliveryReceipt,
}

/// Render topic evidence as indivisible record blocks within a caller budget.
///
/// The ranked order is authoritative, except that a bounded request moves every
/// accepted Constraint ahead of non-Constraints. Starting at the lowest-ranked
/// record, claims degrade through the requirement's explicit ladder until the
/// complete blocks plus their counted receipt fit: full claim, first sentence,
/// title with a retrieval pointer, then omission. Constraints stop at the title
/// tier; if even that protected core cannot fit, retrieval fails loudly.
fn render_topic_records_within(
    state: &AppState,
    records: &[graph::ContextItem],
    max_bytes: Option<usize>,
) -> anyhow::Result<RenderedTopicEvidence> {
    let mut deliveries: Vec<_> = records
        .iter()
        .map(|record| TopicDelivery::new(state, record))
        .collect();
    if max_bytes.is_some() {
        deliveries.sort_by_key(|delivery| usize::from(delivery.record.kind != "Constraint"));
    }

    if let Some(limit) = max_bytes {
        loop {
            let rendered = render_topic_deliveries(&deliveries);
            if rendered.len() <= limit {
                break;
            }
            let Some(delivery) = deliveries
                .iter_mut()
                .rev()
                .find(|delivery| delivery.next_tier().is_some())
            else {
                anyhow::bail!(
                    "context byte budget {limit} is smaller than the protected Constraint titles and counted delivery notice"
                );
            };
            delivery.tier = delivery.next_tier().unwrap();
        }
    }

    let context = render_topic_deliveries(&deliveries);
    let evidence_iris = deliveries
        .iter()
        .filter(|delivery| delivery.tier != ContextRecordDeliveryTier::Omitted)
        .map(|delivery| delivery.record.iri.clone())
        .collect();
    let records = deliveries
        .iter()
        .map(|delivery| ContextRecordDelivery {
            iri: delivery.record.iri.clone(),
            kind: delivery.record.kind.clone(),
            tier: delivery.tier,
            reason: delivery_reason(max_bytes, delivery.tier).into(),
        })
        .collect();
    Ok(RenderedTopicEvidence {
        receipt: ContextDeliveryReceipt {
            max_bytes,
            context_bytes: context.len(),
            records,
        },
        context,
        evidence_iris,
    })
}

fn delivery_reason(max_bytes: Option<usize>, tier: ContextRecordDeliveryTier) -> &'static str {
    match (max_bytes, tier) {
        (None, _) => "unbounded request",
        (_, ContextRecordDeliveryTier::FullClaim) => "complete claim fit within caller byte budget",
        (_, ContextRecordDeliveryTier::FirstSentence) => {
            "complete claim did not fit; first sentence and retrieval pointer fit"
        }
        (_, ContextRecordDeliveryTier::TitleOnly) => {
            "claim did not fit; title and retrieval pointer fit"
        }
        (_, ContextRecordDeliveryTier::Omitted) => {
            "record block did not fit; counted in the delivery notice"
        }
    }
}

fn render_topic_deliveries(deliveries: &[TopicDelivery<'_>]) -> String {
    let mut out = String::new();
    for delivery in deliveries {
        out.push_str(delivery.block());
    }
    if deliveries
        .iter()
        .any(|delivery| delivery.tier != ContextRecordDeliveryTier::FullClaim)
    {
        out.push_str(&delivery_notice(deliveries));
    }
    out
}

fn topic_record_header(record: &graph::ContextItem) -> String {
    format!("\n[{}] {} ({})\n", record.kind, record.label, record.iri)
}

fn topic_record_claim(state: &AppState, record: &graph::ContextItem) -> String {
    let mut claim = String::new();
    graph::render_styled_claim_body(state, record, graph::ClaimStyle::Harness, &mut claim);
    claim
}

/// The first sentence of the first rendered claim line, on a UTF-8 boundary.
fn first_claim_sentence(claim: &str) -> &str {
    let line = claim.lines().next().unwrap_or_default();
    for (index, ch) in line.char_indices() {
        if matches!(ch, '.' | '!' | '?') {
            let end = index + ch.len_utf8();
            if end == line.len() || line[end..].chars().next().is_some_and(char::is_whitespace) {
                return &line[..end];
            }
        }
    }
    line
}

fn delivery_notice(deliveries: &[TopicDelivery<'_>]) -> String {
    let count = |wanted| {
        deliveries
            .iter()
            .filter(|delivery| delivery.tier == wanted)
            .count()
    };
    let mut affected = std::collections::BTreeMap::<&str, usize>::new();
    for delivery in deliveries {
        if delivery.tier != ContextRecordDeliveryTier::FullClaim {
            *affected.entry(&delivery.record.kind).or_default() += 1;
        }
    }
    let kinds = affected
        .into_iter()
        .map(|(kind, n)| format!("{kind}: {n}"))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "\nDelivery under caller byte bound: {} full claim(s), {} first-sentence claim(s), {} title-only record(s), {} omitted record(s) ({kinds}). Search a record title to retrieve its complete claim.\n",
        count(ContextRecordDeliveryTier::FullClaim),
        count(ContextRecordDeliveryTier::FirstSentence),
        count(ContextRecordDeliveryTier::TitleOnly),
        count(ContextRecordDeliveryTier::Omitted),
    )
}

/// The current knowledge records offered to the link path: the bounded
/// inventory plus the topic recall for change obligations.
pub(super) fn current_record_targets(state: &AppState) -> anyhow::Result<Vec<CaptureTarget>> {
    let inventory = graph::relevant_context_snapshot(state, None, 100, false)?;
    let topical =
        graph::relevant_context_snapshot(state, Some("change purpose obligations"), 12, false)?;
    let mut targets: Vec<CaptureTarget> = Vec::new();
    for record in inventory.iter().chain(&topical) {
        if is_record_kind(&record.kind)
            && current_status(state, &record.iri)
                .is_some_and(|status| graph::in_working_set(&status))
            && !targets.iter().any(|target| target.iri == record.iri)
        {
            targets.push(CaptureTarget {
                iri: record.iri.clone(),
                label: record.label.clone(),
                kind: record.kind.clone(),
            });
        }
    }
    Ok(targets)
}

/// Ignore unratified subjects AND inferred incoming links to those subjects.
/// A proposed capture must not invalidate the approval that led to its creation.
pub fn accepted_revision(state: &AppState) -> anyhow::Result<String> {
    accepted_revision_excluding(state, &HashSet::new(), &HashSet::new())
}

/// The accepted revision ignoring `masked` subjects (with the quads that
/// reference them) and the exact `own_quads` a review wrote onto records that
/// existed before it.
pub(super) fn accepted_revision_excluding(
    state: &AppState,
    masked: &HashSet<String>,
    own_quads: &HashSet<String>,
) -> anyhow::Result<String> {
    let graph = GraphNameRef::NamedNode(NamedNodeRef::new(PROJECT_KG_GRAPH_IRI)?);
    let quads = state
        .store
        .quads_for_pattern(None, None, None, Some(graph))
        .collect::<Result<Vec<_>, _>>()?;
    let mut excluded: HashSet<String> = quads
        .iter()
        .filter_map(|q| {
            if q.predicate.as_str() != state.capture.status {
                return None;
            }
            match &q.object {
                Term::Literal(status) if !graph::in_working_set(status.value()) => {
                    Some(q.subject.to_string())
                }
                _ => None,
            }
        })
        .collect();
    excluded.extend(masked.iter().cloned());
    let mut canonical: Vec<String> = quads
        .iter()
        .filter(|q| {
            !excluded.contains(&q.subject.to_string()) && !excluded.contains(&q.object.to_string())
        })
        .map(ToString::to_string)
        .filter(|quad| !own_quads.contains(quad))
        .collect();
    canonical.sort();
    Ok(sha256_hex(canonical.join("\n")))
}
