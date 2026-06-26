import {
  AdrDetailResponse,
  AdrListResponse,
  ChatMessage,
  ChatResponse,
  ChatSessionDetail,
  ChatSessionListResponse,
  GraphImportResponse,
  HealthResponse,
  QueryResponse,
  RequirementDetailResponse,
  RequirementListResponse,
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

async function download(path: string): Promise<Blob> {
  const response = await fetch(`/api/v1${path}`);
  if (!response.ok) {
    const data = await response.json().catch(() => null);
    const message = data?.error ?? `${response.status} ${response.statusText}`;
    throw new Error(message);
  }
  return response.blob();
}

function graphTransferPath(basePath: string, params: { format?: string; graph?: string; mode?: string }) {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value) {
      search.set(key, value);
    }
  }
  const suffix = search.toString();
  return `${basePath}${suffix ? `?${suffix}` : ''}`;
}

export const api = {
  health: () => request<HealthResponse>('/health'),
  listAdrs: () => request<AdrListResponse>('/adrs'),
  getAdr: (num: string) => request<AdrDetailResponse>(`/adrs/${encodeURIComponent(num)}`),
  downloadAdrArchive: () => download('/adrs/archive.zip'),
  listRequirements: () => request<RequirementListResponse>('/requirements'),
  getRequirement: (num: string) =>
    request<RequirementDetailResponse>(`/requirements/${encodeURIComponent(num)}`),
  downloadRequirementArchive: () => download('/requirements/archive.zip'),
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
  exportGraph: (params: { format?: string; graph?: string } = {}) =>
    download(graphTransferPath('/graph/export', params)),
  importGraph: (params: { format?: string; graph?: string; mode?: string }, text: string) =>
    request<GraphImportResponse>(graphTransferPath('/graph/import', params), {
      method: 'POST',
      headers: { 'Content-Type': 'text/plain' },
      body: text,
    }),
};
