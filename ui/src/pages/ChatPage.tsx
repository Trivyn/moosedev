import { useCallback, useEffect, useMemo, useState } from 'react';
import { Alert, Box, CircularProgress, Divider, FormControlLabel, Switch, Tab, Tabs, Typography } from '@mui/material';
import { api } from '../api/client';
import {
  ChatMessage,
  ChatResponse,
  ChatSessionSummary,
  ClarificationReply,
  FocusEntry,
  QueryResponse,
} from '../api/types';
import ChatInput from '../components/chat/ChatInput';
import ChatMessageBubble, { UIChatMessage } from '../components/chat/ChatMessage';
import { describeReply } from '../components/chat/clarification';
import FocusStack from '../components/chat/FocusStack';
import SessionList from '../components/chat/SessionList';
import CytoscapeGraph from '../components/graph/CytoscapeGraph';
import { queryToGraph } from '../components/graph/graphUtils';
import RawResults from '../components/sparql/RawResults';

export default function ChatPage() {
  const [sessions, setSessions] = useState<ChatSessionSummary[]>([]);
  const [sessionId, setSessionId] = useState<string | undefined>();
  const [messages, setMessages] = useState<UIChatMessage[]>([]);
  const [focus, setFocus] = useState<FocusEntry[]>([]);
  const [subgraph, setSubgraph] = useState<QueryResponse | null>(null);
  const [metrics, setMetrics] = useState<unknown>(null);
  const [tab, setTab] = useState(0);
  const [showMooseTraces, setShowMooseTraces] = useState(true);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadSessions = useCallback(async () => {
    const response = await api.listSessions();
    setSessions(response.sessions);
  }, []);

  useEffect(() => {
    loadSessions().catch((err) => setError(err instanceof Error ? err.message : String(err)));
  }, [loadSessions]);

  const graph = useMemo(() => queryToGraph(subgraph, { showMooseTraces }), [showMooseTraces, subgraph]);

  const startNew = () => {
    setSessionId(undefined);
    setMessages([]);
    setFocus([]);
    setSubgraph(null);
    setMetrics(null);
    setError(null);
  };

  const selectSession = async (id: string) => {
    setLoading(true);
    setError(null);
    try {
      const detail = await api.getSession(id);
      setSessionId(detail.session_id);
      setMessages(detail.messages);
      setFocus(detail.focus_stack);
      setSubgraph(detail.session_subgraph);
      setMetrics(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  const deleteSession = async (id: string) => {
    await api.deleteSession(id);
    if (id === sessionId) startNew();
    await loadSessions();
  };

  // Send the visible transcript, not just the last message. MOOSE's session DB
  // is authoritative for state, but the OpenAI-compatible request shape still
  // expects a message list for the current turn. Strip UI-only clarification
  // fields — the wire type is just { role, content }.
  const toWireMessages = (msgs: UIChatMessage[]): ChatMessage[] =>
    msgs.map(({ role, content }) => ({ role, content }));

  // Mark the most-recent assistant clarification (if any) as answered, so its
  // card renders disabled once the reply is in flight.
  const closeOpenClarification = (msgs: UIChatMessage[]): UIChatMessage[] => {
    const lastIdx = [...msgs]
      .map((m, i) => ({ m, i }))
      .reverse()
      .find(({ m }) => m.role === 'assistant' && !!m.clarification)?.i;
    if (lastIdx === undefined) return msgs;
    return msgs.map((m, i) => (i === lastIdx ? { ...m, clarificationAnswered: true } : m));
  };

  // Append the assistant turn — a clarification card when MOOSE paused for
  // clarification, otherwise a normal markdown answer.
  const appendAssistantTurn = (response: ChatResponse) => {
    const choice = response.choices[0];
    if (!choice?.message) return;
    const clarification =
      choice.finish_reason === 'clarification' ? response.moose?.clarification : undefined;
    const msg: UIChatMessage = clarification
      ? { role: 'assistant', content: choice.message.content, clarification }
      : { role: 'assistant', content: choice.message.content };
    setMessages((prev) => [...prev, msg]);
  };

  // Shared post-response side effects (session id, focus stack, subgraph,
  // metrics, session list). Used by both the free-text and reply paths.
  const applyResponseSideEffects = async (response: ChatResponse) => {
    if (response.moose?.session_id) setSessionId(response.moose.session_id);
    setFocus(response.moose?.session_map ?? []);
    setSubgraph(response.moose?.session_subgraph ?? null);
    setMetrics(response.moose?.metrics ?? response.usage);
    await loadSessions();
  };

  const send = async (content: string) => {
    const nextMessages: UIChatMessage[] = [...messages, { role: 'user', content }];
    setMessages(nextMessages);
    setLoading(true);
    setError(null);
    try {
      const response = await api.chat({
        session_id: sessionId,
        messages: toWireMessages(nextMessages),
        include_session_map: true,
        include_metrics: true,
      });
      appendAssistantTurn(response);
      await applyResponseSideEffects(response);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setMessages(nextMessages);
    } finally {
      setLoading(false);
    }
  };

  // Submit a structured reply to a pending clarification. Appends a synthetic
  // user transcript line so the conversation reads naturally, then POSTs the
  // next turn on the same session with `clarification_reply` set.
  const handleClarificationReply = async (reply: ClarificationReply) => {
    if (loading) return;
    const nextMessages: UIChatMessage[] = [
      ...closeOpenClarification(messages),
      { role: 'user', content: describeReply(reply) },
    ];
    setMessages(nextMessages);
    setLoading(true);
    setError(null);
    try {
      const response = await api.chat({
        session_id: sessionId,
        messages: toWireMessages(nextMessages),
        include_session_map: true,
        include_metrics: true,
        clarification_reply: reply,
      });
      appendAssistantTurn(response);
      await applyResponseSideEffects(response);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setMessages(nextMessages);
    } finally {
      setLoading(false);
    }
  };

  // While a clarification card is open, lock free-text input so the user
  // answers through the card (the reply must be keyed to the pending request).
  const lastMessageIsClarification = useMemo(() => {
    const last = messages[messages.length - 1];
    return !!(last && last.role === 'assistant' && last.clarification && !last.clarificationAnswered);
  }, [messages]);

  return (
    <Box sx={{ height: '100%', display: 'flex', overflow: 'hidden' }}>
      <SessionList
        sessions={sessions}
        selectedId={sessionId}
        onNew={startNew}
        onSelect={selectSession}
        onDelete={(id) => deleteSession(id).catch((err) => setError(err instanceof Error ? err.message : String(err)))}
      />
      <Box sx={{ flex: 1, minWidth: 0, display: 'flex', flexDirection: 'column' }}>
        <Box sx={{ p: 1.5, borderBottom: 1, borderColor: 'divider' }}>
          <Typography variant="h6">MOOSE Chat</Typography>
          <Typography variant="caption" color="text.secondary">
            {sessionId ?? 'New session'}
          </Typography>
        </Box>
        {error && (
          <Alert severity="error" onClose={() => setError(null)} sx={{ m: 1 }}>
            {error}
          </Alert>
        )}
        <Box sx={{ flex: 1, overflowY: 'auto', minHeight: 0 }}>
          {messages.length === 0 && !loading && (
            <Box sx={{ height: '100%', display: 'grid', placeItems: 'center', color: 'text.secondary' }}>
              <Typography variant="body2">Ask a question about recorded project knowledge.</Typography>
            </Box>
          )}
          {messages.map((message, index) => (
            <ChatMessageBubble
              key={index}
              message={message}
              onClarificationReply={handleClarificationReply}
            />
          ))}
          {loading && (
            <Box sx={{ display: 'flex', gap: 1, alignItems: 'center', p: 2 }}>
              <CircularProgress size={16} />
              <Typography variant="caption" color="text.secondary">
                Thinking
              </Typography>
            </Box>
          )}
        </Box>
        <ChatInput disabled={loading || lastMessageIsClarification} onSend={send} />
      </Box>
      <Divider orientation="vertical" flexItem />
      <Box sx={{ width: '42%', minWidth: 420, display: 'flex', flexDirection: 'column' }}>
        <Box
          sx={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'space-between',
            borderBottom: 1,
            borderColor: 'divider',
            pr: 1,
          }}
        >
          <Tabs value={tab} onChange={(_, value) => setTab(value)}>
            <Tab label="Graph" />
            <Tab label="Focus" />
            <Tab label="Metrics" />
          </Tabs>
          {tab === 0 && (
            <FormControlLabel
              control={
                <Switch
                  size="small"
                  checked={showMooseTraces}
                  onChange={(event) => setShowMooseTraces(event.target.checked)}
                />
              }
              label="Pipeline traces"
              sx={{
                m: 0,
                ml: 1,
                '.MuiFormControlLabel-label': { typography: 'caption', color: 'text.secondary' },
              }}
            />
          )}
        </Box>
        <Box sx={{ flex: 1, minHeight: 0 }}>
          {tab === 0 && <CytoscapeGraph nodes={graph.nodes} edges={graph.edges} />}
          {tab === 1 && <FocusStack focus={focus} />}
          {tab === 2 && <RawResults value={metrics} />}
        </Box>
      </Box>
    </Box>
  );
}
