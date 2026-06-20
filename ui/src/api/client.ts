import {
  ChatMessage,
  ChatResponse,
  ChatSessionDetail,
  ChatSessionListResponse,
  HealthResponse,
  QueryResponse,
} from './types';

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`/api/v1${path}`, {
    headers: {
      'Content-Type': 'application/json',
      ...(init?.headers ?? {}),
    },
    ...init,
  });
  const data = await response.json().catch(() => null);
  if (!response.ok) {
    const message = data?.error ?? `${response.status} ${response.statusText}`;
    throw new Error(message);
  }
  return data as T;
}

export const api = {
  health: () => request<HealthResponse>('/health'),
  chat: (payload: {
    session_id?: string;
    messages: ChatMessage[];
    include_structured?: boolean;
    include_session_map?: boolean;
    include_metrics?: boolean;
  }) =>
    request<ChatResponse>('/chat', {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  listSessions: () => request<ChatSessionListResponse>('/chat/sessions'),
  getSession: (sessionId: string) =>
    request<ChatSessionDetail>(`/chat/sessions/${encodeURIComponent(sessionId)}`),
  deleteSession: (sessionId: string) =>
    request<{ deleted: boolean }>(`/chat/sessions/${encodeURIComponent(sessionId)}`, {
      method: 'DELETE',
    }),
  sparql: (query: string) =>
    request<QueryResponse>('/sparql/query', {
      method: 'POST',
      body: JSON.stringify({ query }),
    }),
};
