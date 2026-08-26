export interface HealthResponse {
  status: string;
  version: string;
  project_graph: string;
  data_dir: string;
  project_name: string;
  project_root: string;
  llm_configured: boolean;
  llm_assist_level: string;
}

export type StoryStatus = 'draft' | 'published';
export type StoryTrustState = 'generated' | StoryStatus;
export type StoryAssistLevel = 0 | 1;
export type StorySectionKind =
  | 'orientation'
  | 'evolution'
  | 'current_state'
  | 'implementation'
  | 'implications';

export interface StoryGenerateRequest {
  prompt?: string;
  component_iri?: string;
  subject_iri?: string;
  topic?: string;
  recipe_id?: string;
  fresh?: boolean;
  include_checks?: boolean;
  assist_level: StoryAssistLevel;
}

export type StoryRecipeSubject =
  | { type: 'entity'; iri: string }
  | { type: 'topic'; query: string };

export interface StoryRecipeFocus {
  include_record_iris: string[];
  exclude_record_iris: string[];
  include_code_symbols: string[];
  exclude_code_symbols: string[];
  emphasis: StorySectionKind[];
}

export interface StoryRecipe {
  id: string;
  title: string;
  schema_version: 3;
  subject: StoryRecipeSubject;
  goal: string;
  audience: 'reboarding';
  focus: StoryRecipeFocus;
  curator_context?: string;
  status: StoryStatus;
  curator: string;
  updated_at?: string | null;
}

export interface StorySummary {
  id: string;
  title: string;
  subject: StoryRecipeSubject;
  subject_label: string;
  subject_kind: string;
  goal: string;
  audience: 'reboarding';
  status: StoryStatus;
  curator: string;
  updated_at?: string | null;
  drifted?: boolean;
}

export interface StoryListResponse {
  stories: StorySummary[];
}

export interface StoryRecipeResponse {
  recipe: StoryRecipe;
}

export interface StorySubjectCandidate {
  iri: string;
  kind: string;
  label: string;
  description?: string | null;
  /** The graph records nothing about this subject beyond its own existence. */
  no_recorded_knowledge?: boolean;
}

export interface StorySubjectListResponse {
  subjects: StorySubjectCandidate[];
}

export interface StoryEvidenceRelation {
  predicate: string;
  label: string;
  direction: 'outgoing' | 'incoming';
  target_iri: string;
  target_label: string;
  target_kind: string;
}

export interface StoryLiteralProperty {
  predicate: string;
  label: string;
  value: string;
}

export interface StoryEvidenceDetail {
  iri: string;
  title: string;
  kind: string;
  status: string;
  description?: string | null;
  timestamp?: string | null;
  author?: string | null;
  suppressed: boolean;
  properties: StoryLiteralProperty[];
  relations: StoryEvidenceRelation[];
}

export interface StoryCodeAnchor {
  symbol: string;
  label: string;
  entity_iri?: string | null;
  path?: string | null;
  line?: number | null;
}

export interface StoryParagraph {
  text: string;
  citation_iris: string[];
}

export interface StoryNarrativeSection {
  id: string;
  title: string;
  kind: StorySectionKind;
  paragraphs: StoryParagraph[];
}

export interface StoryTimelineEvent {
  id: string;
  title: string;
  kind: string;
  status: string;
  timestamp?: string | null;
  evidence_iri: string;
  relation?: string | null;
  predecessor_iris: string[];
  successor_iris: string[];
  rationale_iris: string[];
}

export interface StoryCoverage {
  entity_count: number;
  current_count: number;
  historical_count: number;
  proposed_count: number;
  code_anchor_count: number;
  dossier_bytes: number;
  subject_families: string[];
  outline_sections: StorySectionKind[];
  truncated: boolean;
}

export interface StoryGap {
  id: string;
  title: string;
  detail: string;
  section_kind?: StorySectionKind | null;
}

export interface StoryCheckOption {
  id: string;
  label: string;
}

export interface StoryCheck {
  id: string;
  question: string;
  options: StoryCheckOption[];
}

export interface StoryRun {
  schema_version: 3;
  recipe_id?: string | null;
  trust_state: StoryTrustState;
  narration_mode: 'symbolic' | 'llm';
  narration_strategy: 'symbolic' | 'single_pass';
  narration_outcome:
    | 'not_requested'
    | 'succeeded'
    | 'unconfigured'
    | 'ineligible'
    | 'timeout'
    | 'provider_error'
    | 'invalid_response';
  narration_failure_reason?:
    | 'packet_too_large'
    | 'invalid_json'
    | 'schema_mismatch'
    | 'citation_mismatch'
    | 'structured_output_unsupported'
    | null;
  narration_coverage?: {
    eligible_entities: number;
    included_entities: number;
    source_groups: number;
    truncated: boolean;
  } | null;
  title: string;
  subject:
    | { type: 'entity'; iri: string; kind: string; label: string }
    | { type: 'topic'; query: string; label: string };
  goal: string;
  curator_context?: string | null;
  brief: StoryParagraph;
  narrative: StoryNarrativeSection[];
  timeline: StoryTimelineEvent[];
  evidence: StoryEvidenceDetail[];
  code_anchors: StoryCodeAnchor[];
  coverage: StoryCoverage;
  gaps: StoryGap[];
  checks: StoryCheck[];
}

export type StoryGenerateResponse =
  | { outcome: 'story'; story: StoryRun }
  | {
      outcome: 'ambiguous';
      prompt: string;
      recipe_id?: string | null;
      candidates: StorySubjectCandidate[];
    };

export interface StoryCheckGradeResponse {
  correct: boolean;
  feedback: string;
  revisit_section_id?: string | null;
  evidence_iris: string[];
}

export interface ComponentCoverage {
  iri: string | null;
  story_component_iri: string | null;
  name: string;
  numerator: number;
  denominator: number;
  coverage: number | null;
  /** Core-surface subset (ratified core roles); 0/0 until roles are ratified. */
  core_numerator: number;
  core_denominator: number;
  undocumented: string[];
}

export interface WhyCoverageResponse {
  components: ComponentCoverage[];
  unmapped: number;
}

export interface Proposal {
  id: string;
  iri: string;
  /**
   * 'link' (pending record → entity edge), 'record' (proposed record), or
   * 'judgment' (pending entity → role/criticality edge).
   */
  kind: 'link' | 'record' | 'judgment';
  label: string;
  subject_iri: string;
  predicate: string;
  target_symbol: string;
  target_path: string;
  /** Local class name for 'record' entries (e.g. ArchitecturalDecision). */
  record_class: string | null;
  /** Role/criticality individual IRI, for 'judgment' entries. */
  target_iri: string;
  /** Classifier confidence literal (e.g. '0.75'), for 'judgment' entries. */
  confidence: string | null;
  /** 'escalated' or 'auto-held', for 'judgment' entries. */
  escalation: string | null;
  /** Subject's human name: record title ('link') or entity code name ('judgment'). */
  subject_name: string;
  /** Subject record's claim (description snippet), for 'link' entries. */
  subject_description: string | null;
  /** Subject entity defining file, for 'judgment' entries. */
  subject_path: string;
  /** Humanized target: logical path ('link') or individual local name ('judgment'). */
  target_display: string;
  evidence: string | null;
  status: string;
  /** Superseded record for a proposed high-stakes replacement. */
  predecessor_iri: string | null;
  predecessor_title: string | null;
  supersession_reason: string | null;
  /** Backend-owned, bounded old/new claim diff. */
  claim_diff: string | null;
  diff_truncated: boolean;
}

export interface ProposalListResponse {
  proposals: Proposal[];
}

export interface ProposalActionResponse {
  id: string;
  status: string;
  entity_iri: string | null;
  entity_name: string | null;
}

export interface AdrWarnings {
  missing_context: string[];
  missing_decision: string[];
  missing_successor: string[];
  missing_reciprocal: string[];
}

export interface AdrSummary {
  num: string;
  title: string;
  status: string;
  date: string;
  author: string;
  iri: string;
  filename: string;
  /** Complete generated detail text used by the shared artifact-list search. */
  search_text: string;
}

export interface AdrListResponse {
  generated_at: string;
  graph_decisions: number;
  adr_files: number;
  index_filename: string;
  warnings: AdrWarnings;
  adrs: AdrSummary[];
}

export interface AdrDetailResponse {
  summary: AdrSummary;
  markdown: string;
}

export interface RequirementWarnings {
  missing_description: string[];
  unlinked_requirements: string[];
}

export interface RequirementSummary {
  num: string;
  title: string;
  status: string;
  addressed: boolean;
  date: string;
  author: string;
  iri: string;
  filename: string;
  related_adrs: number;
  /** Complete generated detail text used by the shared artifact-list search. */
  search_text: string;
}

export interface RequirementListResponse {
  generated_at: string;
  graph_requirements: number;
  requirement_files: number;
  index_filename: string;
  warnings: RequirementWarnings;
  requirements: RequirementSummary[];
}

export interface RequirementDetailResponse {
  summary: RequirementSummary;
  markdown: string;
}

export interface LessonWarnings {
  missing_description: string[];
  unlinked_lessons: string[];
}

export interface LessonSummary {
  num: string;
  title: string;
  status: string;
  date: string;
  author: string;
  iri: string;
  filename: string;
  related_sources: number;
  /** Complete generated detail text used by the shared artifact-list search. */
  search_text: string;
}

export interface LessonListResponse {
  generated_at: string;
  graph_lessons: number;
  lesson_files: number;
  index_filename: string;
  warnings: LessonWarnings;
  lessons: LessonSummary[];
}

export interface LessonDetailResponse {
  summary: LessonSummary;
  markdown: string;
}

export interface ConstraintWarnings {
  missing_description: string[];
  unlinked_constraints: string[];
}

export interface ConstraintSummary {
  num: string;
  title: string;
  status: string;
  date: string;
  author: string;
  iri: string;
  filename: string;
  related_targets: number;
  /** Complete generated detail text used by the shared artifact-list search. */
  search_text: string;
}

export interface ConstraintListResponse {
  generated_at: string;
  graph_constraints: number;
  constraint_files: number;
  index_filename: string;
  warnings: ConstraintWarnings;
  constraints: ConstraintSummary[];
}

export interface ConstraintDetailResponse {
  summary: ConstraintSummary;
  markdown: string;
}

export interface RecordOutgoingEdge {
  predicate: string;
  target_iri: string;
  target_label: string;
  target_kind: string;
}

export interface RecordIncomingEdge {
  predicate: string;
  source_iri: string;
  source_label: string;
  source_kind: string;
}

export interface RecordDetailResponse {
  iri: string;
  kind: string;
  title: string;
  description: string | null;
  status: string | null;
  timestamp: string | null;
  author: string | null;
  story_component_iri: string | null;
  /** Present only for CodeEntity records. A substrate projection, not a graph claim. */
  code: RecordCodeDetail | null;
  outgoing: RecordOutgoingEdge[];
  incoming: RecordIncomingEdge[];
}

/** A 1-based, UTF-8-byte source span. */
export interface SourceSpan {
  start_line: number;
  start_col: number;
  end_line: number;
  end_col: number;
}

export interface RecordCodeDetail {
  symbol: string | null;
  name: string | null;
  entity_kind: string | null;
  logical_path: string | null;
  defined_in_path: string | null;
  signature: string | null;
  source_path: string | null;
  definition: SourceSpan | null;
  source_available: boolean;
  source_unavailable_reason: string | null;
  substrate_stale: boolean;
}

export type SourceScope = 'context' | 'full';

export interface RecordSourceResponse {
  path: string;
  scope: SourceScope;
  start_line: number;
  end_line: number;
  total_lines: number;
  truncated: boolean;
  definition: SourceSpan | null;
  text: string;
}

export interface ChatMessage {
  role: 'system' | 'user' | 'assistant';
  content: string;
}

export interface QueryValue {
  type: 'uri' | 'bnode' | 'literal' | 'unknown';
  value: string;
  datatype?: string;
  lang?: string;
}

export interface QueryBinding {
  [key: string]: QueryValue;
}

export interface QueryResponse {
  query_type: 'SELECT' | 'ASK' | 'CONSTRUCT';
  head?: { vars: string[] };
  results?: { bindings: QueryBinding[] };
  boolean?: boolean;
  triples?: Array<{
    subject: QueryValue;
    predicate: QueryValue;
    object: QueryValue;
  }>;
}

export interface GraphImportResponse {
  format: 'turtle' | 'ntriples' | 'nquads';
  mode: 'patch' | 'replace';
  graphs: string[];
  parsed_quad_count: number;
  duplicate_input_count: number;
  inserted_quad_count: number;
  skipped_existing_count: number;
  removed_quad_count: number;
}

export interface FocusEntry {
  iri: string;
  class_iri: string;
  label: string;
  salience: number;
  introduced_at: number;
  last_mentioned: number;
}

// ── Clarification round-trip ──────────────────────────────────────────────
// Mirrors moose::clarification types. Tagged-enum shapes match Rust serde
// `#[serde(tag = "kind", content = "data")]`.

export type SlotKind =
  | { kind: 'UnknownTerm'; data: { noun: string } }
  | { kind: 'UnknownEntity' }
  | { kind: 'LowConfidenceTerm'; data: { noun: string } }
  | { kind: 'UnresolvedEntity'; data: { surface: string } }
  | {
      kind: 'UnresolvedModifier';
      data: {
        raw_text: string;
        target_class: string | null;
        sort_dimension?: string | null;
      };
    }
  | { kind: 'PickCandidate' }
  | { kind: 'DefineClassOrProperty'; data: { iri: string } };

export type ReplyAction =
  | { kind: 'AltLabel'; data: { surface: string; target_iri: string } }
  | { kind: 'HiddenLabel'; data: { surface: string; target_iri: string } }
  | { kind: 'Definition'; data: { target_iri: string; definition: string } }
  | { kind: 'PickCandidate'; data: { iri: string } }
  | { kind: 'Decline' };

export type AgentRef =
  | { kind: 'Human'; data: { user_id?: string | null } }
  | { kind: 'Jockey'; data: { agent_id: string } };

export type ExpectedKind = 'Class' | 'ObjectProperty' | 'DatatypeProperty' | 'Instance';

export interface ClarificationCandidate {
  iri: string;
  local_name: string;
  label?: string;
  kind: ExpectedKind;
  score: number;
}

export interface ClarificationRequest {
  id: string;
  session_id: string;
  turn_number: number;
  question: string;
  original_question: string;
  slot_kind: SlotKind;
  missing_field?: string | null;
  expected_kinds: ExpectedKind[];
  candidates: ClarificationCandidate[];
  trigger: string;
  created_at: string;
  unresolved_surface?: string | null;
}

export interface ClarificationReply {
  id: string;
  user_text: string;
  action: ReplyAction;
  remember_for_user: boolean;
  agent: AgentRef;
}

export interface ChatResponse {
  id: string;
  object: string;
  created: number;
  model: string;
  choices: Array<{
    index: number;
    message: ChatMessage;
    finish_reason: string;
  }>;
  usage: {
    prompt_tokens: number;
    completion_tokens: number;
    total_tokens: number;
  };
  moose?: {
    session_id: string;
    structured?: unknown;
    session_map?: FocusEntry[];
    metrics?: unknown;
    /** Present when MOOSE paused the turn for clarification. The companion
     * `choices[0].finish_reason` is `"clarification"` on the same response. */
    clarification?: ClarificationRequest;
    session_subgraph?: QueryResponse;
  };
}

export interface ChatSessionSummary {
  session_id: string;
  turn_count: number;
  created_at: number;
  updated_at: number;
  last_user_message?: string;
}

export interface ChatSessionListResponse {
  sessions: ChatSessionSummary[];
  count: number;
}

export interface ChatSessionDetail {
  session_id: string;
  turn_count: number;
  messages: ChatMessage[];
  focus_stack: FocusEntry[];
  session_subgraph: QueryResponse;
}

export interface GraphNode {
  id: string;
  label: string;
  type: string;
  properties?: GraphProperty[];
}

export interface GraphEdge {
  id: string;
  source: string;
  target: string;
  label: string;
  type: string;
  predicate?: string;
  properties?: GraphProperty[];
}

export interface GraphProperty {
  predicate: string;
  values: QueryValue[];
}
