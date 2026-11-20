import { useCallback, useEffect, useState } from 'react';
import {
  Alert,
  Box,
  Button,
  Card,
  CardContent,
  Chip,
  CircularProgress,
  MenuItem,
  Stack,
  TextField,
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

/** When an inbox item was proposed: compact local time, the stored value on hover. */
function ProposedAt({ timestamp }: { timestamp: string | null }) {
  if (!timestamp) return null;
  const date = new Date(timestamp);
  const shown = Number.isNaN(date.valueOf())
    ? timestamp
    : date.toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
  return (
    <Tooltip title={`Proposed ${timestamp}`}>
      <Typography variant="caption" color="text.secondary" sx={{ whiteSpace: 'nowrap' }}>
        {shown}
      </Typography>
    </Tooltip>
  );
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

/**
 * Inbox order. 'priority' is the triage order (records, then links, then
 * judgments with escalated groups first); the others are reader overrides.
 */
type SortMode = 'priority' | 'newest' | 'oldest' | 'name';

const SORT_LABELS: Record<SortMode, string> = {
  priority: 'Priority (escalated first)',
  newest: 'Newest first',
  oldest: 'Oldest first',
  name: 'Name (A–Z)',
};

const SORT_STORAGE_KEY = 'moosedev.ratifications.sort';

// A per-browser convenience: unavailable storage falls back to the triage order.
function readSortMode(): SortMode {
  try {
    const stored = localStorage.getItem(SORT_STORAGE_KEY);
    return stored && stored in SORT_LABELS ? (stored as SortMode) : 'priority';
  } catch {
    return 'priority';
  }
}

function storeSortMode(mode: SortMode) {
  try {
    localStorage.setItem(SORT_STORAGE_KEY, mode);
  } catch {
    // Storage blocked: the choice lasts for this visit only.
  }
}

/** Proposal time in epoch ms, or null when missing or unparseable. */
function timeOf(proposal: Proposal): number | null {
  const ms = proposal.timestamp ? Date.parse(proposal.timestamp) : NaN;
  return Number.isNaN(ms) ? null : ms;
}

/** A group's time under a time sort: its newest member, or its oldest. */
function groupTime(group: Proposal[], mode: SortMode): number | null {
  const times = group.map(timeOf).filter((t): t is number => t !== null);
  if (times.length === 0) return null;
  return mode === 'oldest' ? Math.min(...times) : Math.max(...times);
}

/**
 * Compare by time under 'newest'/'oldest' (undated entries last either way),
 * falling back to name; by name alone otherwise.
 */
function compareBy(
  mode: SortMode,
  a: { time: number | null; name: string },
  b: { time: number | null; name: string },
): number {
  if ((mode === 'newest' || mode === 'oldest') && a.time !== b.time) {
    if (a.time === null) return 1;
    if (b.time === null) return -1;
    return mode === 'newest' ? b.time - a.time : a.time - b.time;
  }
  return a.name.localeCompare(b.name);
}

/** Case-insensitive substring match over every field the page shows. */
function matchesFilter(proposal: Proposal, needle: string): boolean {
  if (!needle) return true;
  return [
    proposal.label,
    proposal.iri,
    proposal.subject_iri,
    proposal.subject_name,
    proposal.subject_description,
    proposal.subject_path,
    proposal.record_class,
    proposal.predicate,
    proposal.target_display,
    proposal.target_symbol,
    proposal.target_path,
    localName(proposal.target_iri),
    proposal.evidence,
    proposal.escalation,
    proposal.predecessor_title,
  ].some((field) => field?.toLowerCase().includes(needle));
}

/** Judgments grouped for batch triage: one group per predicate → target. */
function judgmentGroups(judgments: Proposal[], mode: SortMode): Array<[string, Proposal[]]> {
  const groups = new Map<string, Proposal[]>();
  for (const judgment of judgments) {
    const axis = judgment.predicate === 'playsRole' ? 'role' : 'criticality';
    const key = `${axis}: ${localName(judgment.target_iri)}`;
    const group = groups.get(key) ?? [];
    group.push(judgment);
    groups.set(key, group);
  }
  // Rows sorted by entity name (or time) so a group reads like a file listing.
  for (const group of groups.values()) {
    group.sort((a, b) =>
      compareBy(
        mode,
        { time: timeOf(a), name: a.subject_name },
        { time: timeOf(b), name: b.subject_name },
      ),
    );
  }
  const entries = [...groups.entries()];
  if (mode !== 'priority') {
    return entries.sort(([aKey, a], [bKey, b]) =>
      compareBy(mode, { time: groupTime(a, mode), name: aKey }, { time: groupTime(b, mode), name: bKey }),
    );
  }
  // Triage order: escalated groups first, larger groups first within each tier.
  return entries.sort(([, a], [, b]) => {
    const aEsc = a.some((j) => j.escalation === 'escalated') ? 0 : 1;
    const bEsc = b.some((j) => j.escalation === 'escalated') ? 0 : 1;
    return aEsc - bEsc || b.length - a.length;
  });
}

/** A top-level inbox card: one proposed record, or one record's link proposals. */
type InboxCard =
  | { kind: 'record'; proposal: Proposal }
  | { kind: 'links'; subjectIri: string; group: Proposal[] };

/**
 * Records and link groups as one card list. Priority keeps records before
 * links; the other modes interleave them so time order holds across kinds.
 */
function inboxCards(records: Proposal[], links: Proposal[], mode: SortMode): InboxCard[] {
  // One card per source record: a capture that touched 20 files is one
  // decision to triage, not 20.
  const linkGroups = new Map<string, Proposal[]>();
  for (const link of links) {
    const group = linkGroups.get(link.subject_iri) ?? [];
    group.push(link);
    linkGroups.set(link.subject_iri, group);
  }
  const cards: InboxCard[] = [
    ...records.map((proposal): InboxCard => ({ kind: 'record', proposal })),
    ...[...linkGroups.entries()].map(
      ([subjectIri, group]): InboxCard => ({ kind: 'links', subjectIri, group }),
    ),
  ];
  if (mode === 'priority') return cards;

  for (const card of cards) {
    if (card.kind === 'links') {
      card.group.sort((a, b) =>
        compareBy(
          mode,
          { time: timeOf(a), name: a.target_display || a.target_symbol },
          { time: timeOf(b), name: b.target_display || b.target_symbol },
        ),
      );
    }
  }
  const key = (card: InboxCard) =>
    card.kind === 'record'
      ? { time: timeOf(card.proposal), name: card.proposal.label }
      : { time: groupTime(card.group, mode), name: card.group[0].subject_name };
  return cards.sort((a, b) => compareBy(mode, key(a), key(b)));
}

export default function RatificationsPage({ onNavigateRecord }: RatificationsPageProps) {
  const [proposals, setProposals] = useState<Proposal[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [sort, setSort] = useState<SortMode>(readSortMode);
  const [filter, setFilter] = useState('');

  const changeSort = (mode: SortMode) => {
    setSort(mode);
    storeSortMode(mode);
  };

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

  const needle = filter.trim().toLowerCase();
  const shown = proposals.filter((p) => matchesFilter(p, needle));
  const records = shown.filter((p) => p.kind === 'record');
  const links = shown.filter((p) => p.kind === 'link');
  const judgments = shown.filter((p) => p.kind === 'judgment');
  const cards = inboxCards(records, links, sort);

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
        <>
          <Stack direction="row" spacing={1.5} alignItems="center" sx={{ mb: 2 }}>
            <TextField
              size="small"
              type="search"
              label="Filter"
              placeholder="Title, path, kind, evidence…"
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
              sx={{ flexGrow: 1, maxWidth: 420 }}
            />
            <TextField
              select
              size="small"
              label="Sort"
              value={sort}
              onChange={(event) => changeSort(event.target.value as SortMode)}
              sx={{ minWidth: 210 }}
            >
              {(Object.keys(SORT_LABELS) as SortMode[]).map((mode) => (
                <MenuItem key={mode} value={mode}>
                  {SORT_LABELS[mode]}
                </MenuItem>
              ))}
            </TextField>
            {needle && (
              <Typography variant="caption" color="text.secondary">
                {shown.length} of {proposals.length} shown
              </Typography>
            )}
          </Stack>
          {shown.length === 0 ? (
            <Typography variant="body2" color="text.secondary">
              Nothing pending matches “{filter.trim()}”.
            </Typography>
          ) : (
            <Stack spacing={1.5}>
              {cards.map((card) => {
                if (card.kind === 'record') {
                  const proposal = card.proposal;
                  return (
                    <Card key={proposal.id} variant="outlined">
                      <CardContent>
                        <Stack direction="row" spacing={1} alignItems="center" sx={{ mb: 1 }}>
                          <Chip size="small" color="secondary" label={proposal.record_class ?? 'Record'} />
                          <Typography variant="subtitle2">{proposal.label}</Typography>
                          <Box sx={{ flexGrow: 1 }} />
                          <ProposedAt timestamp={proposal.timestamp} />
                        </Stack>
                        {proposal.evidence && (
                          <Typography
                            variant="caption"
                            color="text.secondary"
                            sx={{
                              display: '-webkit-box',
                              WebkitLineClamp: 3,
                              WebkitBoxOrient: 'vertical',
                              overflow: 'hidden',
                            }}
                          >
                            {proposal.evidence}
                          </Typography>
                        )}
                        {proposal.predecessor_iri && (
                          <Box sx={{ mt: 1 }}>
                            <Typography variant="caption" color="text.secondary">
                              Replaces{' '}
                              <Box
                                component="span"
                                sx={{ color: 'primary.main', cursor: 'pointer' }}
                                onClick={() => onNavigateRecord(proposal.predecessor_iri!)}
                              >
                                {proposal.predecessor_title ?? `record ${shortIri(proposal.predecessor_iri)}`}
                              </Box>
                              {proposal.supersession_reason
                                ? ` · reason: ${proposal.supersession_reason}`
                                : ''}
                            </Typography>
                            {proposal.claim_diff && (
                              <Box
                                component="pre"
                                sx={{
                                  bgcolor: 'action.hover',
                                  borderRadius: 1,
                                  fontFamily: 'monospace',
                                  fontSize: '0.75rem',
                                  overflowX: 'auto',
                                  p: 1,
                                  whiteSpace: 'pre-wrap',
                                }}
                              >
                                {proposal.claim_diff}
                              </Box>
                            )}
                          </Box>
                        )}
                        <Typography variant="body2" sx={{ mt: 0.5 }}>
                          <Box
                            component="span"
                            sx={{ color: 'primary.main', cursor: 'pointer' }}
                            onClick={() => onNavigateRecord(proposal.iri)}
                          >
                            view full record
                          </Box>{' '}
                          — accepting ratifies it into the working set
                        </Typography>
                        <Stack direction="row" spacing={1} sx={{ mt: 1.5 }}>
                          <Button
                            size="small"
                            variant="contained"
                            startIcon={<CheckIcon />}
                            disabled={busy !== null}
                            onClick={() => act(proposal.id, 'accept')}
                          >
                            Accept
                          </Button>
                          <Button
                            size="small"
                            variant="outlined"
                            color="inherit"
                            startIcon={<CloseIcon />}
                            disabled={busy !== null}
                            onClick={() => act(proposal.id, 'reject')}
                          >
                            Reject
                          </Button>
                        </Stack>
                      </CardContent>
                    </Card>
                  );
                }
                const { subjectIri, group } = card;
                return (
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
                        {group[0].subject_description && (
                          <Typography
                            variant="caption"
                            color="text.secondary"
                            sx={{
                              display: '-webkit-box',
                              WebkitLineClamp: 2,
                              WebkitBoxOrient: 'vertical',
                              overflow: 'hidden',
                              mt: 0.5,
                            }}
                          >
                            {group[0].subject_description}
                          </Typography>
                        )}
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
                              <ProposedAt timestamp={link.timestamp} />
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
                );
              })}

              {judgments.length > 0 && (
                <>
                  <Typography variant="h6" sx={{ mt: 2 }}>
                    Judgments ({judgments.length})
                  </Typography>
                  <Typography variant="body2" color="text.secondary">
                    Classifier-proposed roles and criticalities. They never nudge and take effect only
                    as advisory badges until ratified; a ratified criticality-high additionally gates
                    edits. Triage by group{sort === 'priority' ? ' — escalated groups first' : ''}.
                  </Typography>
                  {judgmentGroups(judgments, sort).map(([groupKey, group]) => (
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
                              <ProposedAt timestamp={judgment.timestamp} />
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
        </>
      )}
    </Box>
  );
}
