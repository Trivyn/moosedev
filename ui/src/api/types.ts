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
  outgoing: RecordOutgoingEdge[];
  incoming: RecordIncomingEdge[];
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
