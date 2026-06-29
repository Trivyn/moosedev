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
    clarification?: unknown;
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
