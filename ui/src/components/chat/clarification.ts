import { AgentRef, ClarificationReply } from '../../api/types';

/** Default agent: anonymous Human. The MOOSEDev backend normalises this to a
 * stable `"default"` user_id (src/api/handlers/chat.rs) so learned surface
 * forms accumulate in the user overlay across sessions without requiring
 * per-user authentication. */
export function anonymousHuman(): AgentRef {
  return { kind: 'Human', data: { user_id: null } };
}

/** Render a `ClarificationReply` as a short human-readable transcript line.
 * Used to keep the chat history readable when the structured reply is
 * submitted via `ClarificationCard` rather than typed into the input. */
export function describeReply(reply: ClarificationReply): string {
  switch (reply.action.kind) {
    case 'AltLabel':
      return `altLabel: "${reply.action.data.surface}" → <${reply.action.data.target_iri}>`;
    case 'HiddenLabel':
      return `hiddenLabel: "${reply.action.data.surface}" → <${reply.action.data.target_iri}>`;
    case 'Definition':
      return `Definition for <${reply.action.data.target_iri}>: ${reply.action.data.definition}`;
    case 'PickCandidate':
      return `Pick: <${reply.action.data.iri}>`;
    case 'Decline':
      return '(declined the clarification)';
    default:
      return reply.user_text || '(reply)';
  }
}
