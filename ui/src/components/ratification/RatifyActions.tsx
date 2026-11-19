import { useEffect, useState } from 'react';
import { Alert, Box, Button, CircularProgress, Stack, Typography } from '@mui/material';
import CheckIcon from '@mui/icons-material/Check';
import CloseIcon from '@mui/icons-material/Close';
import { api } from '../../api/client';
import { Proposal } from '../../api/types';

interface RatifyActionsProps {
  /** The record's UUID — also its proposal id in the ratification inbox. */
  uuid: string;
  /** The record's lifecycle status, raw ('proposed') or rendered ('Proposed'). */
  status: string | null | undefined;
  /** Called after a successful accept or reject, so the view can reload. */
  onRatified: () => void;
  onNavigateRecord?: (iri: string) => void;
}

export function isProposed(status: string | null | undefined): boolean {
  return status?.trim().toLowerCase() === 'proposed';
}

/**
 * Accept / Reject for a proposed record, on the record's own view. The same
 * inbox endpoints as the Ratifications page; a replacement also shows what it
 * replaces and the claim diff, since accepting it supersedes the predecessor.
 */
export default function RatifyActions({ uuid, status, onRatified, onNavigateRecord }: RatifyActionsProps) {
  const proposed = isProposed(status);
  const [entry, setEntry] = useState<Proposal | null>(null);
  const [busy, setBusy] = useState<'accept' | 'reject' | null>(null);
  const [error, setError] = useState<string | null>(null);

  // The inbox entry carries the replacement context (predecessor, reason, diff).
  // Missing context never hides the actions: the endpoints are the authority.
  useEffect(() => {
    setEntry(null);
    setError(null);
    if (!proposed) return;
    let cancelled = false;
    api
      .listProposals('proposed')
      .then((response) => {
        if (!cancelled) {
          setEntry(response.proposals.find((p) => p.kind === 'record' && p.id === uuid) ?? null);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [uuid, proposed]);

  if (!proposed) return null;

  const act = async (kind: 'accept' | 'reject') => {
    setBusy(kind);
    setError(null);
    try {
      if (kind === 'accept') {
        await api.acceptProposal(uuid);
      } else {
        await api.rejectProposal(uuid);
      }
      onRatified();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  };

  return (
    <Alert
      severity="info"
      icon={false}
      action={
        <Stack direction="row" spacing={1} alignItems="center">
          <Button
            size="small"
            variant="contained"
            startIcon={busy === 'accept' ? <CircularProgress color="inherit" size={14} /> : <CheckIcon />}
            disabled={busy !== null}
            onClick={() => act('accept')}
          >
            Accept
          </Button>
          <Button
            size="small"
            variant="outlined"
            color="inherit"
            startIcon={busy === 'reject' ? <CircularProgress color="inherit" size={14} /> : <CloseIcon />}
            disabled={busy !== null}
            onClick={() => act('reject')}
          >
            Reject
          </Button>
        </Stack>
      }
    >
      <Typography variant="body2">
        Proposed — awaiting ratification. Accepting ratifies it into the working set; rejecting
        declines it and keeps it for audit.
      </Typography>
      {entry?.predecessor_iri && (
        <Box sx={{ mt: 1 }}>
          <Typography variant="body2">
            Accepting replaces{' '}
            <Box
              component="span"
              sx={{ color: 'primary.main', cursor: onNavigateRecord ? 'pointer' : 'default', fontWeight: 600 }}
              onClick={() => onNavigateRecord?.(entry.predecessor_iri!)}
            >
              {entry.predecessor_title ?? entry.predecessor_iri}
            </Box>
            {entry.supersession_reason ? ` · reason: ${entry.supersession_reason}` : ''}
          </Typography>
          {entry.claim_diff && (
            <Box
              component="pre"
              sx={{
                bgcolor: 'action.hover',
                borderRadius: 1,
                fontFamily: 'monospace',
                fontSize: '0.75rem',
                overflowX: 'auto',
                p: 1,
                mb: 0,
                whiteSpace: 'pre-wrap',
              }}
            >
              {entry.claim_diff}
            </Box>
          )}
        </Box>
      )}
      {error && (
        <Typography variant="body2" color="error" sx={{ mt: 1 }}>
          {error}
        </Typography>
      )}
    </Alert>
  );
}
