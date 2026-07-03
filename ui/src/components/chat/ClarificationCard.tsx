import { Alert, Box, Button, Chip, Stack, Typography } from '@mui/material';
import HelpOutlineIcon from '@mui/icons-material/HelpOutline';
import {
  ClarificationCandidate,
  ClarificationReply,
  ClarificationRequest,
  ReplyAction,
  SlotKind,
} from '../../api/types';
import { anonymousHuman } from './clarification';

interface ClarificationCardProps {
  request: ClarificationRequest;
  /** Called when the user picks a candidate or declines. The handler is
   * responsible for sending the next chat turn. */
  onReply: (reply: ClarificationReply) => void;
  /** Disabled once a reply has been submitted (so the user can't double-fire
   * while the next turn is in flight). */
  disabled?: boolean;
}

/** UI affordance label for the candidate row. */
function candidateLabel(slot: SlotKind): string {
  switch (slot.kind) {
    case 'UnresolvedModifier':
      return 'Pick a property:';
    case 'UnresolvedEntity':
    case 'UnknownEntity':
      return 'Pick an entity:';
    case 'UnknownTerm':
    case 'LowConfidenceTerm':
    case 'PickCandidate':
    default:
      return 'Did you mean:';
  }
}

/** The user's surface form this pick would teach MOOSE (as a `skos:altLabel`),
 * or null when the slot carries nothing persistable. When present, the pick is
 * remembered in the user overlay so the same phrasing resolves without
 * re-prompting in future sessions. */
function persistenceSurface(request: ClarificationRequest): string | null {
  if (request.unresolved_surface?.trim()) {
    return request.unresolved_surface;
  }
  switch (request.slot_kind.kind) {
    case 'UnknownTerm':
    case 'LowConfidenceTerm':
      return request.slot_kind.data.noun;
    case 'UnresolvedEntity':
      return request.slot_kind.data.surface;
    case 'UnresolvedModifier':
      return request.slot_kind.data.sort_dimension || request.slot_kind.data.raw_text || null;
    default:
      return null;
  }
}

/**
 * Clarification card.
 *
 * The user picks one of the candidates MOOSE proposed, or declines. There's no
 * free-text "paste an IRI" input — if MOOSE has no candidates it's not
 * actionable, so we hide the picker and offer Decline only.
 *
 * A teachable pick (one whose slot carries the user's surface form) is always
 * remembered in the user overlay (`remember_for_user: true`) so learned
 * terminology persists across sessions. There's no per-pick toggle: MOOSEDev
 * has no ontologist review queue, and the overlay is user-owned local memory
 * that never touches the shipped ontology. A plain disambiguation (no surface
 * to teach) sends `false` — it writes no triple regardless.
 */
export default function ClarificationCard({ request, onReply, disabled }: ClarificationCardProps) {
  const candidates = request.candidates;
  const canPersistPick = !!persistenceSurface(request);

  const submit = (action: ReplyAction, userText: string, remember: boolean) => {
    onReply({
      id: request.id,
      user_text: userText,
      action,
      remember_for_user: remember,
      agent: anonymousHuman(),
    });
  };

  const onPickCandidate = (c: ClarificationCandidate) => {
    submit({ kind: 'PickCandidate', data: { iri: c.iri } }, `Pick: ${c.label || c.local_name}`, canPersistPick);
  };

  const onDecline = () => {
    submit({ kind: 'Decline' }, '(declined)', false);
  };

  return (
    <Box
      sx={{
        border: (theme) => `1px solid ${theme.palette.divider}`,
        borderRadius: 1,
        p: 1.5,
        backgroundColor: (theme) =>
          theme.palette.mode === 'dark' ? 'rgba(255,255,255,0.04)' : 'rgba(0,0,0,0.02)',
      }}
    >
      <Stack direction="row" spacing={1} alignItems="center" sx={{ mb: 1 }}>
        <HelpOutlineIcon fontSize="small" color="primary" aria-hidden />
        <Typography variant="body2" sx={{ fontWeight: 600 }}>
          {request.question}
        </Typography>
      </Stack>

      {candidates.length > 0 ? (
        <Box sx={{ mb: 0.5 }}>
          <Typography variant="caption" color="text.secondary">
            {candidateLabel(request.slot_kind)}
          </Typography>
          <Stack direction="row" spacing={0.5} sx={{ mt: 0.5, flexWrap: 'wrap', gap: 0.5 }}>
            {candidates.map((c) => (
              <Chip
                key={c.iri}
                label={c.label || c.local_name}
                size="small"
                variant="outlined"
                onClick={() => !disabled && onPickCandidate(c)}
                disabled={disabled}
                sx={{ cursor: disabled ? 'default' : 'pointer' }}
              />
            ))}
          </Stack>
        </Box>
      ) : (
        <Alert severity="info" sx={{ mb: 0.5 }}>
          No suggestions available — try rephrasing your question, or decline to dismiss this
          prompt.
        </Alert>
      )}

      <Stack direction="row" justifyContent="flex-end" sx={{ mt: 1 }}>
        <Button size="small" onClick={onDecline} disabled={disabled}>
          Decline
        </Button>
      </Stack>
    </Box>
  );
}
