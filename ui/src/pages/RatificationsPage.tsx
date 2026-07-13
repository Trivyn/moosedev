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
  Tooltip,
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

/** Full local name of an IRI (e.g. 'core-algorithm') — never truncated. */
function localName(iri: string): string {
  return iri.slice(Math.max(iri.lastIndexOf('/'), iri.lastIndexOf('#')) + 1) || iri;
}

/** Taxonomy targets per judgment axis, for the recategorize control. */
const ROLE_TARGETS = [
  'core-algorithm',
  'domain-logic',
  'boundary',
  'glue',
  'boilerplate',
  'generated',
];
const CRITICALITY_TARGETS = ['high', 'standard', 'low'];

function alternativeTargets(judgment: Proposal): string[] {
  const all = judgment.predicate === 'playsRole' ? ROLE_TARGETS : CRITICALITY_TARGETS;
  return all.filter((t) => t !== localName(judgment.target_iri));
}

/** Judgments grouped for batch triage: one group per predicate → target. */
function judgmentGroups(judgments: Proposal[]): Array<[string, Proposal[]]> {
  const groups = new Map<string, Proposal[]>();
  for (const judgment of judgments) {
    const axis = judgment.predicate === 'playsRole' ? 'role' : 'criticality';
    const key = `${axis}: ${localName(judgment.target_iri)}`;
    const group = groups.get(key) ?? [];
    group.push(judgment);
    groups.set(key, group);
  }
  // Escalated groups first, larger groups first within each tier; rows sorted
  // by entity name so a group reads like a file listing.
  for (const group of groups.values()) {
    group.sort((a, b) => a.subject_name.localeCompare(b.subject_name));
  }
  return [...groups.entries()].sort(([, a], [, b]) => {
    const aEsc = a.some((j) => j.escalation === 'escalated') ? 0 : 1;
    const bEsc = b.some((j) => j.escalation === 'escalated') ? 0 : 1;
    return aEsc - bEsc || b.length - a.length;
  });
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

  // Recategorize: the human corrects the classifier's target — the proposal is
  // rejected (audit-preserved) and a human-authored judgment materializes.
  const recategorize = async (id: string, target: string) => {
    if (!target) return;
    setBusy(id);
    setError(null);
    try {
      await api.recategorizeProposal(id, target);
      refresh();
    } catch (err) {
      setError(toMessage(err));
    } finally {
      setBusy(null);
    }
  };

  // Batch triage for a judgment group: sequential client-side loop over the
  // existing endpoints; per-row failures surface, the rest proceed.
  const actAll = async (group: Proposal[], kind: 'accept' | 'reject') => {
    setBusy('batch');
    setError(null);
    const failures: string[] = [];
    for (const proposal of group) {
      try {
        if (kind === 'accept') {
          await api.acceptProposal(proposal.id);
        } else {
          await api.rejectProposal(proposal.id);
        }
      } catch (err) {
        failures.push(`${proposal.label}: ${toMessage(err)}`);
      }
    }
    if (failures.length > 0) {
      setError(`${failures.length} of ${group.length} failed — ${failures[0]}`);
    }
    refresh();
    setBusy(null);
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

  const records = proposals.filter((p) => p.kind === 'record');
  const links = proposals.filter((p) => p.kind === 'link');
  const judgments = proposals.filter((p) => p.kind === 'judgment');
  // One card per source record: a capture that touched 20 files is one
  // decision to triage, not 20.
  const linkGroups = new Map<string, Proposal[]>();
  for (const link of links) {
    const group = linkGroups.get(link.subject_iri) ?? [];
    group.push(link);
    linkGroups.set(link.subject_iri, group);
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
          {records.map((proposal) => (
            <Card key={proposal.id} variant="outlined">
              <CardContent>
                <Stack direction="row" spacing={1} alignItems="center" sx={{ mb: 1 }}>
                  <Chip size="small" color="secondary" label={proposal.record_class ?? 'Record'} />
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

          {[...linkGroups.entries()].map(([subjectIri, group]) => (
            <Card key={subjectIri} variant="outlined">
              <CardContent>
                <Stack direction="row" spacing={1} alignItems="center" sx={{ mb: 0.5 }}>
                  <Chip size="small" color="primary" label={group[0].predicate} />
                  <Box
                    component="span"
                    title={subjectIri}
                    sx={{ color: 'primary.main', cursor: 'pointer', fontWeight: 600 }}
                    onClick={() => onNavigateRecord(subjectIri)}
                  >
                    “{group[0].subject_name || `record ${shortIri(subjectIri)}`}”
                  </Box>
                  <Typography variant="caption" color="text.secondary">
                    would link to {group.length} code entit{group.length === 1 ? 'y' : 'ies'}
                  </Typography>
                  <Box sx={{ flexGrow: 1 }} />
                  <Button
                    size="small"
                    variant="contained"
                    startIcon={<CheckIcon />}
                    disabled={busy !== null}
                    onClick={() => actAll(group, 'accept')}
                  >
                    Accept all
                  </Button>
                  <Button
                    size="small"
                    variant="outlined"
                    color="inherit"
                    startIcon={<CloseIcon />}
                    disabled={busy !== null}
                    onClick={() => actAll(group, 'reject')}
                  >
                    Reject all
                  </Button>
                </Stack>
                <Stack spacing={0.5} sx={{ mt: 1 }}>
                  {group.map((link) => (
                    <Stack
                      key={link.id}
                      direction="row"
                      spacing={1}
                      alignItems="center"
                      sx={{ pl: 1 }}
                    >
                      <Tooltip title={link.target_symbol}>
                        <Typography
                          variant="caption"
                          sx={{ fontFamily: 'monospace', fontWeight: 600, whiteSpace: 'nowrap' }}
                        >
                          {link.target_display || link.target_symbol}
                        </Typography>
                      </Tooltip>
                      <Typography
                        variant="caption"
                        color="text.secondary"
                        noWrap
                        sx={{ flexGrow: 1, minWidth: 0, fontFamily: 'monospace' }}
                      >
                        {link.target_path}
                      </Typography>
                      <Tooltip title={link.evidence ?? ''}>
                        <Typography variant="caption" color="text.secondary" noWrap>
                          {link.evidence}
                        </Typography>
                      </Tooltip>
                      <Button
                        size="small"
                        disabled={busy !== null}
                        onClick={() => act(link.id, 'accept')}
                      >
                        Accept
                      </Button>
                      <Button
                        size="small"
                        color="inherit"
                        disabled={busy !== null}
                        onClick={() => act(link.id, 'reject')}
                      >
                        Reject
                      </Button>
                    </Stack>
                  ))}
                </Stack>
              </CardContent>
            </Card>
          ))}

          {judgments.length > 0 && (
            <>
              <Typography variant="h6" sx={{ mt: 2 }}>
                Judgments ({judgments.length})
              </Typography>
              <Typography variant="body2" color="text.secondary">
                Classifier-proposed roles and criticalities. They never nudge and take effect only
                as advisory badges until ratified; a ratified criticality-high additionally gates
                edits. Triage by group — escalated groups first.
              </Typography>
              {judgmentGroups(judgments).map(([groupKey, group]) => (
                <Card key={groupKey} variant="outlined">
                  <CardContent>
                    <Stack direction="row" spacing={1} alignItems="center" sx={{ mb: 1 }}>
                      <Chip size="small" color="secondary" label={groupKey} />
                      <Typography variant="caption" color="text.secondary">
                        {group.length} proposal{group.length === 1 ? '' : 's'}
                      </Typography>
                      {group.filter((j) => j.escalation === 'escalated').length > 0 && (
                        <Chip
                          size="small"
                          color="warning"
                          label={`${group.filter((j) => j.escalation === 'escalated').length} escalated`}
                        />
                      )}
                      <Box sx={{ flexGrow: 1 }} />
                      <Button
                        size="small"
                        variant="contained"
                        startIcon={<CheckIcon />}
                        disabled={busy !== null}
                        onClick={() => actAll(group, 'accept')}
                      >
                        Accept all
                      </Button>
                      <Button
                        size="small"
                        variant="outlined"
                        color="inherit"
                        startIcon={<CloseIcon />}
                        disabled={busy !== null}
                        onClick={() => actAll(group, 'reject')}
                      >
                        Reject all
                      </Button>
                    </Stack>
                    <Stack spacing={0.5}>
                      {group.map((judgment) => (
                        <Stack
                          key={judgment.id}
                          direction="row"
                          spacing={1}
                          alignItems="center"
                          sx={{ pl: 1 }}
                        >
                          <Box
                            component="span"
                            title={judgment.subject_iri}
                            sx={{
                              color: 'primary.main',
                              cursor: 'pointer',
                              fontFamily: 'monospace',
                              fontSize: '0.85rem',
                              fontWeight: 600,
                              whiteSpace: 'nowrap',
                            }}
                            onClick={() => onNavigateRecord(judgment.subject_iri)}
                          >
                            {judgment.subject_name || shortIri(judgment.subject_iri)}
                          </Box>
                          <Typography
                            variant="caption"
                            color="text.secondary"
                            sx={{ fontFamily: 'monospace', whiteSpace: 'nowrap' }}
                          >
                            {judgment.subject_path}
                          </Typography>
                          {judgment.escalation === 'escalated' && (
                            <Chip size="small" color="warning" label="!" sx={{ height: 16 }} />
                          )}
                          <Tooltip title={judgment.evidence ?? judgment.label}>
                            <Typography
                              variant="caption"
                              color="text.secondary"
                              noWrap
                              sx={{ flexGrow: 1, minWidth: 0 }}
                            >
                              {judgment.evidence ?? judgment.label} ({judgment.confidence ?? '?'})
                            </Typography>
                          </Tooltip>
                          <Button
                            size="small"
                            disabled={busy !== null}
                            onClick={() => act(judgment.id, 'accept')}
                          >
                            Accept
                          </Button>
                          <Button
                            size="small"
                            color="inherit"
                            disabled={busy !== null}
                            onClick={() => act(judgment.id, 'reject')}
                          >
                            Reject
                          </Button>
                          <Tooltip title="Recategorize: reject the classifier's target and ratify your correction in one step">
                            <Box
                              component="select"
                              value=""
                              disabled={busy !== null}
                              onChange={(e: { target: { value: string } }) =>
                                recategorize(judgment.id, e.target.value)
                              }
                              sx={{
                                fontSize: '0.75rem',
                                border: '1px solid',
                                borderColor: 'divider',
                                borderRadius: 1,
                                background: 'transparent',
                                color: 'text.secondary',
                                px: 0.5,
                              }}
                            >
                              <option value="">→ …</option>
                              {alternativeTargets(judgment).map((target) => (
                                <option key={target} value={target}>
                                  {target}
                                </option>
                              ))}
                            </Box>
                          </Tooltip>
                        </Stack>
                      ))}
                    </Stack>
                  </CardContent>
                </Card>
              ))}
            </>
          )}
        </Stack>
      )}
    </Box>
  );
}
