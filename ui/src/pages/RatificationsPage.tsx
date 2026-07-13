import { useCallback, useEffect, useState } from 'react';
import {
  Alert,
  Box,
  Button,
  Card,
  CardContent,
  Chip,
  CircularProgress,
  Stack,
  Typography,
} from '@mui/material';
import CheckIcon from '@mui/icons-material/Check';
import CloseIcon from '@mui/icons-material/Close';
import { api } from '../api/client';
import { Proposal } from '../api/types';

interface RatificationsPageProps {
  onNavigateRecord: (iri: string) => void;
}

function toMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function shortIri(iri: string): string {
  const tail = iri.slice(Math.max(iri.lastIndexOf('/'), iri.lastIndexOf('#')) + 1);
  return tail ? tail.slice(0, 8) : iri;
}

export default function RatificationsPage({ onNavigateRecord }: RatificationsPageProps) {
  const [proposals, setProposals] = useState<Proposal[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const refresh = useCallback(() => {
    api
      .listProposals('proposed')
      .then((response) => setProposals(response.proposals))
      .catch((err) => setError(toMessage(err)));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const act = async (id: string, kind: 'accept' | 'reject') => {
    setBusy(id);
    setError(null);
    try {
      if (kind === 'accept') {
        await api.acceptProposal(id);
      } else {
        await api.rejectProposal(id);
      }
      refresh();
    } catch (err) {
      setError(toMessage(err));
    } finally {
      setBusy(null);
    }
  };

  if (error && !proposals) {
    return (
      <Alert severity="error" sx={{ m: 2 }}>
        {error}
      </Alert>
    );
  }
  if (!proposals) {
    return (
      <Box sx={{ height: '100%', display: 'grid', placeItems: 'center' }}>
        <CircularProgress size={18} />
      </Box>
    );
  }

  return (
    <Box sx={{ p: 3, height: '100%', overflow: 'auto' }}>
      <Typography variant="h5" gutterBottom>
        Ratifications
      </Typography>
      <Typography variant="body2" color="text.secondary" sx={{ mb: 2 }}>
        Proposed links and records awaiting review. Accepting a link materializes the record →
        entity edge (it then counts toward why-coverage); accepting a record ratifies it into the
        working set. Reject to decline — nothing is created, and the proposal is preserved for
        audit.
      </Typography>
      {error && (
        <Alert severity="error" sx={{ mb: 2 }}>
          {error}
        </Alert>
      )}
      {proposals.length === 0 ? (
        <Typography variant="body2" color="text.secondary">
          The inbox is empty — nothing pending ratification.
        </Typography>
      ) : (
        <Stack spacing={1.5}>
          {proposals.map((proposal) => (
            <Card key={proposal.id} variant="outlined">
              <CardContent>
                {proposal.kind === 'record' ? (
                  <>
                    <Stack direction="row" spacing={1} alignItems="center" sx={{ mb: 1 }}>
                      <Chip
                        size="small"
                        color="secondary"
                        label={proposal.record_class ?? 'Record'}
                      />
                      <Typography variant="subtitle2">{proposal.label}</Typography>
                    </Stack>
                    <Typography variant="body2" sx={{ mb: 0.5 }}>
                      <Box
                        component="span"
                        sx={{ color: 'primary.main', cursor: 'pointer' }}
                        onClick={() => onNavigateRecord(proposal.iri)}
                      >
                        proposed record {shortIri(proposal.iri)}
                      </Box>{' '}
                      would join the working set
                    </Typography>
                  </>
                ) : (
                  <>
                    <Stack direction="row" spacing={1} alignItems="center" sx={{ mb: 1 }}>
                      <Chip size="small" color="primary" label={proposal.predicate} />
                      <Typography variant="subtitle2" sx={{ fontFamily: 'monospace' }}>
                        {proposal.target_symbol}
                      </Typography>
                      <Typography variant="caption" color="text.secondary">
                        {proposal.target_path}
                      </Typography>
                    </Stack>
                    <Typography variant="body2" sx={{ mb: 0.5 }}>
                      <Box
                        component="span"
                        sx={{ color: 'primary.main', cursor: 'pointer' }}
                        onClick={() => onNavigateRecord(proposal.subject_iri)}
                      >
                        record {shortIri(proposal.subject_iri)}
                      </Box>{' '}
                      would {proposal.predicate} this entity
                    </Typography>
                  </>
                )}
                {proposal.evidence && (
                  <Typography variant="caption" color="text.secondary">
                    {proposal.evidence}
                  </Typography>
                )}
                <Stack direction="row" spacing={1} sx={{ mt: 1.5 }}>
                  <Button
                    size="small"
                    variant="contained"
                    startIcon={<CheckIcon />}
                    disabled={busy === proposal.id}
                    onClick={() => act(proposal.id, 'accept')}
                  >
                    Accept
                  </Button>
                  <Button
                    size="small"
                    variant="outlined"
                    color="inherit"
                    startIcon={<CloseIcon />}
                    disabled={busy === proposal.id}
                    onClick={() => act(proposal.id, 'reject')}
                  >
                    Reject
                  </Button>
                </Stack>
              </CardContent>
            </Card>
          ))}
        </Stack>
      )}
    </Box>
  );
}
