export interface HealthResponse {
  status: string;
  version: string;
  project_graph: string;
  data_dir: string;
  project_name: string;
  project_root: string;
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
