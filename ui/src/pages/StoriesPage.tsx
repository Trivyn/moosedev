import { FormEvent, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Alert,
  Autocomplete,
  Box,
  Button,
  Card,
  CardActionArea,
  CardContent,
  Chip,
  CircularProgress,
  Divider,
  FormControl,
  FormControlLabel,
  IconButton,
  InputLabel,
  MenuItem,
  Paper,
  Radio,
  RadioGroup,
  Select,
  Stack,
  Tab,
  Tabs,
  TextField,
  Tooltip,
  Typography,
} from '@mui/material';
import AddIcon from '@mui/icons-material/Add';
import ArrowDownwardIcon from '@mui/icons-material/ArrowDownward';
import ArrowUpwardIcon from '@mui/icons-material/ArrowUpward';
import AutoStoriesIcon from '@mui/icons-material/AutoStories';
import DeleteOutlineIcon from '@mui/icons-material/DeleteOutline';
import EditOutlinedIcon from '@mui/icons-material/EditOutlined';
import PublishIcon from '@mui/icons-material/Publish';
import SaveOutlinedIcon from '@mui/icons-material/SaveOutlined';
import { api } from '../api/client';
import { isWorkingSetStatus } from '../utils/lifecycle';
import {
  StoryAssistLevel,
  StoryBeatIntent,
  StoryBeatRecipe,
  StoryCheckGradeResponse,
  StoryGenerateRequest,
  StoryGenerateResponse,
  StoryListResponse,
  StoryRecipe,
  StoryRecipeSubject,
  StoryRun,
  StoryStatus,
  StorySubjectCandidate,
  StorySummary,
  StoryTrustState,
} from '../api/types';

interface StoriesPageProps {
  onNavigateRecord: (iri: string) => void;
  initialComponentIri?: string | null;
  onDirtyChange?: (dirty: boolean) => void;
}

type StorySelectionRequest = Omit<StoryGenerateRequest, 'assist_level' | 'include_checks'>;

const intentLabels: Record<StoryBeatIntent, string> = {
  purpose: 'Purpose',
  boundary: 'Boundary',
  'core-code': 'Core code',
  governance: 'Decisions & constraints',
  risk: 'Risks & extension points',
};
const canonicalIntentOrder: StoryBeatIntent[] = ['purpose', 'boundary', 'core-code', 'governance', 'risk'];

const trustColors: Record<StoryTrustState, 'default' | 'info' | 'success'> = {
  generated: 'info',
  draft: 'default',
  published: 'success',
};

let fallbackBeatSequence = 0;

function uuidEntropy(): string {
  try {
    const uuid = globalThis.crypto?.randomUUID?.();
    if (uuid) return uuid;
  } catch {
    // Fall through for restricted/non-secure browser contexts.
  }
  try {
    const bytes = new Uint8Array(16);
    globalThis.crypto?.getRandomValues?.(bytes);
    if (bytes.some(Boolean)) return [...bytes].map((byte) => byte.toString(16).padStart(2, '0')).join('');
  } catch {
    // Fall through to a process-local collision-resistant fallback.
  }
  fallbackBeatSequence += 1;
  const random = Array.from({ length: 4 }, () => Math.random().toString(36).slice(2, 10)).join('');
  return `${Date.now().toString(36)}-${fallbackBeatSequence.toString(36)}-${random}`;
}

function newBeatId(): string {
  return `beat-${uuidEntropy()}`;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function TrustBadge({ state }: { state: StoryTrustState }) {
  return <Chip size="small" color={trustColors[state]} label={`${state[0].toUpperCase()}${state.slice(1)} Story`} />;
}

function storyId(title: string) {
  const stem = title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-|-$/g, '')
    .slice(0, 48);
  return `${stem || 'story'}-${uuidEntropy()}`;
}

function recipeFromRun(run: StoryRun): StoryRecipe {
  const subject: StoryRecipeSubject = run.subject.type === 'entity'
    ? { type: 'entity', iri: run.subject.iri }
    : { type: 'topic', query: run.subject.query };
  return {
    id: run.recipe_id || storyId(run.title),
    title: run.title,
    schema_version: 2,
    subject,
    goal: run.goal,
    audience: 'reboarding',
    beats: run.beats.map((beat) => ({
      id: beat.id,
      title: beat.title,
      intent: beat.intent,
      record_iris: beat.evidence
        .filter((item) => run.subject.type !== 'entity' || item.iri !== run.subject.iri)
        .map((item) => item.iri),
      code_symbols: beat.code_anchors.map((item) => item.symbol),
      curator_note: beat.curator_note ?? undefined,
    })),
    status: 'draft',
    curator: 'maintainer',
  };
}

function storyStructureFingerprint(story: StoryRun): string {
  return JSON.stringify({
    recipe_id: story.recipe_id ?? null,
    trust_state: story.trust_state,
    title: story.title,
    subject: story.subject,
    goal: story.goal,
    overview: story.overview,
    gaps: story.gaps,
    beats: story.beats.map((beat) => ({
      id: beat.id,
      title: beat.title,
      intent: beat.intent,
      evidence: beat.evidence,
      code_anchors: beat.code_anchors,
      gap: beat.gap ?? null,
      curator_note: beat.curator_note ?? null,
    })),
  });
}

function applyAssistedNarration(symbolic: StoryRun, assisted: StoryRun): StoryRun | null {
  if (storyStructureFingerprint(symbolic) !== storyStructureFingerprint(assisted)) return null;
  return {
    ...symbolic,
    narration_mode: assisted.narration_mode,
    narration_outcome: assisted.narration_outcome,
    beats: symbolic.beats.map((beat, index) => ({
      ...beat,
      narrative: assisted.beats[index].narrative,
    })),
  };
}

function storySubjectIdentity(subject: StoryRun['subject']): string {
  return subject.type === 'entity' ? subject.iri : `topic:${subject.query}`;
}

function storySelection(subject: StoryRun['subject']): StorySelectionRequest {
  return subject.type === 'entity'
    ? { subject_iri: subject.iri }
    : { topic: subject.query };
}

function filterStorySubjects(options: StorySubjectCandidate[], query: string) {
  const normalized = query.trim().toLocaleLowerCase();
  if (!normalized) return options;
  return options.filter((option) =>
    option.label.toLocaleLowerCase().includes(normalized) ||
    option.kind.toLocaleLowerCase().includes(normalized) ||
    option.description?.toLocaleLowerCase().includes(normalized),
  );
}

function narrationNotice(story: StoryRun, assisting: boolean) {
  if (assisting) {
    return {
      severity: 'info' as const,
      text: 'This grounded Story is ready now. MOOSEDev is asking the configured LLM to make the explanations easier to read; the evidence and structure will not change.',
    };
  }
  if (story.narration_mode === 'llm' && story.narration_outcome === 'succeeded') {
    return {
      severity: 'info' as const,
      text: 'The configured LLM turned the evidence shown here into the explanations you are reading. MOOSEDev still selected the Story structure, sources, gaps, and checks deterministically from the project graph.',
    };
  }
  if (story.narration_outcome === 'not_requested') {
    return {
      severity: 'success' as const,
      text: 'This prose is a deterministic summary assembled from current project knowledge; no LLM narration was used.',
    };
  }
  const reason: Record<StoryRun['narration_outcome'], string> = {
    not_requested: '',
    succeeded: '',
    unconfigured: 'no narration provider is configured',
    ineligible: 'the evidence could not be safely packaged for narration',
    timeout: 'the narration request timed out',
    provider_error: 'the narration provider returned an error',
    invalid_response: 'the narration response failed grounding validation',
  };
  return {
    severity: 'warning' as const,
    text: `LLM narration was not used because ${reason[story.narration_outcome]}. The deterministic symbolic Story is shown instead.`,
  };
}

function parseAnchors(value: string): string[] {
  return value
    .split(/[\n,]+/)
    .map((anchor) => anchor.trim())
    .filter(Boolean);
}

function storyEditorValidation(recipe: StoryRecipe) {
  const unanchoredBeats = recipe.beats.filter(
    (beat) =>
      !(beat.intent === 'boundary' && recipe.subject.type === 'entity') &&
      beat.record_iris.length === 0 &&
      beat.code_symbols.length === 0,
  );
  const exceedsBeatLimit = recipe.beats.length > 5;
  const intentIndexes = recipe.beats.map((beat) => canonicalIntentOrder.indexOf(beat.intent));
  const hasDuplicateIntents = new Set(recipe.beats.map((beat) => beat.intent)).size !== recipe.beats.length;
  const hasNonCanonicalIntentOrder = intentIndexes.some(
    (intentIndex, index) => index > 0 && intentIndex <= intentIndexes[index - 1],
  );
  const referenceErrors = recipe.beats.flatMap((beat) => {
    const errors: string[] = [];
    if (beat.record_iris.length > 6) errors.push(`${beat.title} has more than six record IRIs`);
    if (beat.code_symbols.length > 6) errors.push(`${beat.title} has more than six code symbols`);
    if (new Set(beat.record_iris).size !== beat.record_iris.length) errors.push(`${beat.title} has duplicate record IRIs`);
    if (new Set(beat.code_symbols).size !== beat.code_symbols.length) errors.push(`${beat.title} has duplicate code symbols`);
    return errors;
  });

  return {
    unanchoredBeats,
    exceedsBeatLimit,
    hasDuplicateIntents,
    hasNonCanonicalIntentOrder,
    referenceErrors,
    hasPublishedStructureErrors:
      recipe.beats.length < 3 ||
      exceedsBeatLimit ||
      unanchoredBeats.length > 0 ||
      hasDuplicateIntents ||
      hasNonCanonicalIntentOrder,
  };
}

function StoryLibrary({
  data,
  onOpen,
  onEdit,
  busy,
}: {
  data: StoryListResponse;
  onOpen: (story: StorySummary) => void;
  onEdit: (story: StorySummary) => void;
  busy: boolean;
}) {
  const groups = useMemo(
    () => ({
      published: data.stories.filter((story) => story.status === 'published'),
      draft: data.stories.filter((story) => story.status === 'draft'),
    }),
    [data],
  );

  if (data.stories.length === 0) {
    return (
      <Typography variant="body2" color="text.secondary">
        No saved Stories yet. Generate one above, then save it for curation.
      </Typography>
    );
  }

  return (
    <Stack spacing={2.5}>
      {(['published', 'draft'] as StoryStatus[]).map((status) =>
        groups[status].length > 0 ? (
          <Box key={status}>
            <Typography variant="overline" color="text.secondary">
              {status}
            </Typography>
            <Stack spacing={1}>
              {groups[status].map((story) => (
                <Card key={story.id} variant="outlined">
                  <CardActionArea disabled={busy} onClick={() => onOpen(story)}>
                    <CardContent sx={{ pb: 1.5 }}>
                      <Stack direction="row" spacing={1} justifyContent="space-between" alignItems="flex-start">
                        <Typography variant="subtitle2">{story.title}</Typography>
                        <TrustBadge state={story.status} />
                      </Stack>
                      <Typography variant="caption" color="text.secondary" sx={{ display: 'block', mt: 0.75 }}>
                        {story.subject_kind}: {story.subject_label} · {story.beat_count} beats
                      </Typography>
                      {story.drifted && (
                        <Chip size="small" color="warning" variant="outlined" label="Changed since curation" sx={{ mt: 1 }} />
                      )}
                    </CardContent>
                  </CardActionArea>
                  <Divider />
                  <Button disabled={busy} size="small" startIcon={<EditOutlinedIcon />} onClick={() => onEdit(story)} sx={{ m: 0.5 }}>
                    Curate
                  </Button>
                </Card>
              ))}
            </Stack>
          </Box>
        ) : null,
      )}
    </Stack>
  );
}

function StoryEditor({
  recipe,
  busy,
  onChange,
  onSave,
  onPublish,
  onClose,
  dirty,
}: {
  recipe: StoryRecipe;
  busy: boolean;
  onChange: (recipe: StoryRecipe) => void;
  onSave: () => void;
  onPublish: () => void;
  onClose: () => void;
  dirty: boolean;
}) {
  const {
    unanchoredBeats,
    exceedsBeatLimit,
    hasDuplicateIntents,
    hasNonCanonicalIntentOrder,
    referenceErrors,
    hasPublishedStructureErrors,
  } = storyEditorValidation(recipe);
  const hasReferenceErrors = referenceErrors.length > 0;
  const updateBeat = (index: number, patch: Partial<StoryBeatRecipe>) => {
    const beats = [...recipe.beats];
    beats[index] = { ...beats[index], ...patch };
    onChange({ ...recipe, beats });
  };
  const moveBeat = (index: number, direction: -1 | 1) => {
    const destination = index + direction;
    if (destination < 0 || destination >= recipe.beats.length) return;
    const beats = [...recipe.beats];
    [beats[index], beats[destination]] = [beats[destination], beats[index]];
    onChange({ ...recipe, beats });
  };
  const removeBeat = (index: number) =>
    onChange({ ...recipe, beats: recipe.beats.filter((_, beatIndex) => beatIndex !== index) });
  const addBeat = () =>
    onChange({
      ...recipe,
      beats: [
        ...recipe.beats,
        {
          id: newBeatId(),
          title: 'New beat',
          intent: 'risk',
          record_iris: [],
          code_symbols: [],
          curator_note: '',
        },
      ],
    });

  return (
    <Paper variant="outlined" sx={{ p: { xs: 2, md: 3 } }}>
      <Stack spacing={2}>
        <Stack direction="row" alignItems="center" justifyContent="space-between">
          <Box>
            <Typography variant="h6">Curate Story</Typography>
            <Typography variant="caption" color="text.secondary">
              Recipes store the route and annotations; project claims remain in the knowledge graph.
            </Typography>
          </Box>
          <Button disabled={busy || dirty} onClick={onClose}>Close</Button>
        </Stack>
        <TextField disabled={busy} label="Title" value={recipe.title} onChange={(event) => onChange({ ...recipe, title: event.target.value })} />
        <TextField
          disabled
          label="Story subject"
          value={recipe.subject.type === 'entity' ? recipe.subject.iri : recipe.subject.query}
          helperText={recipe.subject.type === 'entity' ? 'Exact project entity' : 'Saved topic query'}
        />
        <TextField
          disabled={busy}
          label="Learning goal"
          value={recipe.goal}
          onChange={(event) => onChange({ ...recipe, goal: event.target.value })}
          multiline
        />
        <Stack spacing={1.5}>
          {recipe.beats.map((beat, index) => (
            <Paper key={beat.id} variant="outlined" sx={{ p: 2 }}>
              <Stack direction={{ xs: 'column', md: 'row' }} spacing={1.5} alignItems={{ md: 'flex-start' }}>
                <Stack direction="row" spacing={0.25}>
                  <Tooltip title="Move up"><span><IconButton aria-label={`Move ${beat.title} up`} size="small" disabled={busy || index === 0} onClick={() => moveBeat(index, -1)}><ArrowUpwardIcon fontSize="small" /></IconButton></span></Tooltip>
                  <Tooltip title="Move down"><span><IconButton aria-label={`Move ${beat.title} down`} size="small" disabled={busy || index === recipe.beats.length - 1} onClick={() => moveBeat(index, 1)}><ArrowDownwardIcon fontSize="small" /></IconButton></span></Tooltip>
                </Stack>
                <TextField
                  size="small"
                  disabled={busy}
                  label={`Beat ${index + 1}`}
                  value={beat.title}
                  onChange={(event) => updateBeat(index, { title: event.target.value })}
                  sx={{ flex: 1 }}
                />
                <FormControl disabled={busy} size="small" sx={{ minWidth: 190 }}>
                  <InputLabel>Intent</InputLabel>
                  <Select label="Intent" value={beat.intent} onChange={(event) => updateBeat(index, { intent: event.target.value as StoryBeatIntent })}>
                    {Object.entries(intentLabels).map(([value, label]) => <MenuItem key={value} value={value}>{label}</MenuItem>)}
                  </Select>
                </FormControl>
                <Tooltip title="Remove beat"><span><IconButton disabled={busy} aria-label={`Remove ${beat.title}`} size="small" color="error" onClick={() => removeBeat(index)}><DeleteOutlineIcon fontSize="small" /></IconButton></span></Tooltip>
              </Stack>
              <TextField
                fullWidth
                size="small"
                disabled={busy}
                label="Curator note"
                placeholder="Optional transition or emphasis; not an authoritative project claim"
                value={beat.curator_note ?? ''}
                onChange={(event) => updateBeat(index, { curator_note: event.target.value })}
                multiline
                sx={{ mt: 1.5 }}
              />
              <Stack direction={{ xs: 'column', md: 'row' }} spacing={1.5} sx={{ mt: 1.5 }}>
                <TextField
                  fullWidth
                  size="small"
                  disabled={busy}
                  label={`Record IRIs for ${beat.title}`}
                  helperText="One per line or comma-separated"
                  value={beat.record_iris.join('\n')}
                  onChange={(event) => updateBeat(index, { record_iris: parseAnchors(event.target.value) })}
                  multiline
                  minRows={2}
                />
                <TextField
                  fullWidth
                  size="small"
                  disabled={busy}
                  label={`Code symbols for ${beat.title}`}
                  helperText="Stable symbols, one per line or comma-separated"
                  value={beat.code_symbols.join('\n')}
                  onChange={(event) => updateBeat(index, { code_symbols: parseAnchors(event.target.value) })}
                  multiline
                  minRows={2}
                />
              </Stack>
            </Paper>
          ))}
        </Stack>
        <Button disabled={busy} variant="outlined" startIcon={<AddIcon />} onClick={addBeat} sx={{ alignSelf: 'flex-start' }}>Add beat</Button>
        {exceedsBeatLimit ? (
          <Alert severity="warning">Stories may contain at most five beats. Remove a beat before saving.</Alert>
        ) : recipe.beats.length < 3 ? (
          <Alert severity="info">Drafts may contain zero to five beats; publishing requires at least three.</Alert>
        ) : null}
        {unanchoredBeats.length > 0 ? (
          <Alert severity="warning">
            Every published beat needs at least one current record or code anchor. An entity Story's Boundary beat may use the subject itself. Missing: {unanchoredBeats.map((beat) => beat.title).join(', ')}.
          </Alert>
        ) : null}
        {hasDuplicateIntents || hasNonCanonicalIntentOrder ? (
          <Alert severity="warning">
            Published Story beats must use unique intents in this order: Purpose, Boundary, Core code, Decisions &amp; constraints, Risks &amp; extension points.
          </Alert>
        ) : null}
        {hasReferenceErrors ? (
          <Alert severity="error">
            Each beat may reference at most six unique records and six unique code symbols. {referenceErrors.join('. ')}.
          </Alert>
        ) : null}
        {dirty ? <Alert severity="info">Save changes before closing or starting another Story.</Alert> : null}
        <Stack direction="row" spacing={1}>
          <Button
            variant="contained"
            startIcon={<SaveOutlinedIcon />}
            disabled={
              busy ||
              exceedsBeatLimit ||
              hasReferenceErrors ||
              (recipe.status === 'published' && hasPublishedStructureErrors)
            }
            onClick={onSave}
          >
            {recipe.status === 'published' ? 'Save changes' : 'Save draft'}
          </Button>
          <Button
            variant="outlined"
            startIcon={<PublishIcon />}
            disabled={busy || hasPublishedStructureErrors || hasReferenceErrors}
            onClick={onPublish}
          >
            Publish
          </Button>
        </Stack>
      </Stack>
    </Paper>
  );
}

function StoryReader({
  story,
  onNavigateRecord,
  onSaveDraft,
  onCurate,
  onClose,
  onGenerateFresh,
  busy,
  assisting,
}: {
  story: StoryRun;
  onNavigateRecord: (iri: string) => void;
  onSaveDraft: () => void;
  onCurate: () => void;
  onClose: () => void;
  onGenerateFresh: () => void;
  busy: boolean;
  assisting: boolean;
}) {
  const [selected, setSelected] = useState<Record<string, string>>({});
  const [results, setResults] = useState<Record<string, StoryCheckGradeResponse>>({});
  const [gradeErrors, setGradeErrors] = useState<Record<string, string>>({});
  const [grading, setGrading] = useState<Record<string, boolean>>({});
  const gradeRequestRef = useRef<Record<string, number>>({});
  const checkIdentity = `${story.recipe_id ?? storySubjectIdentity(story.subject)}\u0001${story.checks
    .map((check) => check.id)
    .join('\u0000')}`;

  useEffect(() => {
    gradeRequestRef.current = {};
    setSelected({});
    setResults({});
    setGradeErrors({});
    setGrading({});
    return () => {
      gradeRequestRef.current = {};
    };
  }, [checkIdentity]);

  const grade = async (checkId: string) => {
    const optionId = selected[checkId];
    if (!optionId) return;
    const request = (gradeRequestRef.current[checkId] ?? 0) + 1;
    gradeRequestRef.current[checkId] = request;
    setGrading((current) => ({ ...current, [checkId]: true }));
    setGradeErrors((current) => {
      const next = { ...current };
      delete next[checkId];
      return next;
    });
    try {
      const result = await api.gradeStoryCheck({
        check_id: checkId,
        selected_option_ids: [optionId],
      });
      if (gradeRequestRef.current[checkId] !== request) return;
      setResults((current) => ({ ...current, [checkId]: result }));
      if (!result.correct && result.revisit_beat_id) {
        document.getElementById(`story-beat-${result.revisit_beat_id}`)?.scrollIntoView({ behavior: 'smooth' });
      }
    } catch (err) {
      if (gradeRequestRef.current[checkId] === request) {
        setGradeErrors((current) => ({
          ...current,
          [checkId]: errorMessage(err),
        }));
      }
    } finally {
      if (gradeRequestRef.current[checkId] === request) {
        setGrading((current) => ({ ...current, [checkId]: false }));
      }
    }
  };

  const selectAnswer = (checkId: string, optionId: string) => {
    gradeRequestRef.current[checkId] = (gradeRequestRef.current[checkId] ?? 0) + 1;
    setSelected((current) => ({ ...current, [checkId]: optionId }));
    setGrading((current) => ({ ...current, [checkId]: false }));
    setResults((current) => {
      const next = { ...current };
      delete next[checkId];
      return next;
    });
    setGradeErrors((current) => {
      const next = { ...current };
      delete next[checkId];
      return next;
    });
  };

  const provenance = narrationNotice(story, assisting);

  return (
    <Stack spacing={3}>
      <Paper variant="outlined" sx={{ p: { xs: 2, md: 3 } }}>
        <Stack direction={{ xs: 'column', md: 'row' }} spacing={2} justifyContent="space-between" alignItems={{ md: 'flex-start' }}>
          <Box>
            <Stack direction="row" spacing={1} alignItems="center" useFlexGap flexWrap="wrap">
              <TrustBadge state={story.trust_state} />
              <Chip
                size="small"
                variant="outlined"
                label={story.narration_mode === 'llm' ? 'LLM-assisted narration' : 'Symbolic extract'}
              />
              {assisting ? <Chip size="small" color="info" variant="outlined" label="Improving narration…" /> : null}
            </Stack>
            <Typography variant="h4" sx={{ mt: 1 }}>{story.title}</Typography>
            <Typography variant="subtitle1" color="text.secondary">{story.subject.label}</Typography>
          </Box>
          <Stack direction="row" spacing={1}>
            <Button disabled={busy} onClick={onClose}>All Stories</Button>
            {story.trust_state === 'generated' ? (
              <Button disabled={busy} variant="outlined" startIcon={<SaveOutlinedIcon />} onClick={onSaveDraft}>Save as draft</Button>
            ) : (
              <>
                <Button disabled={busy} variant="outlined" onClick={onGenerateFresh}>Generate fresh</Button>
                <Button disabled={busy} variant="outlined" startIcon={<EditOutlinedIcon />} onClick={onCurate}>Curate</Button>
              </>
            )}
          </Stack>
        </Stack>
        <Typography variant="body1" sx={{ mt: 2, maxWidth: 900 }}>{story.overview}</Typography>
        <Typography variant="body2" color="text.secondary" sx={{ mt: 1 }}><strong>Goal:</strong> {story.goal}</Typography>
        <Alert severity={provenance.severity} variant="outlined" sx={{ mt: 2, maxWidth: 900 }}>
          {provenance.text}
        </Alert>
      </Paper>

      <Stack spacing={2}>
        {story.beats.map((beat, index) => (
          <Paper id={`story-beat-${beat.id}`} key={beat.id} variant="outlined" sx={{ p: { xs: 2, md: 3 } }}>
            <Typography variant="overline" color="primary">{index + 1} · {intentLabels[beat.intent]}</Typography>
            <Typography variant="h6" gutterBottom>{beat.title}</Typography>
            <Typography variant="body1" sx={{ whiteSpace: 'pre-wrap', maxWidth: 900 }}>{beat.narrative}</Typography>
            {beat.curator_note ? (
              <Alert severity="info" variant="outlined" sx={{ mt: 2 }}>
                <strong>Maintainer note (non-authoritative):</strong> {beat.curator_note}
              </Alert>
            ) : null}
            {beat.gap && <Alert severity="warning" sx={{ mt: 2 }}><strong>Knowledge gap:</strong> {beat.gap}</Alert>}
            <Stack direction="row" spacing={1} useFlexGap flexWrap="wrap" sx={{ mt: 2 }}>
              {beat.evidence.map((item) => (
                <Chip
                  key={item.iri}
                  clickable
                  variant="outlined"
                  color={isWorkingSetStatus(item.status) ? 'primary' : 'warning'}
                  label={`${item.kind}: ${item.title} · ${item.status || 'unknown'}`}
                  onClick={() => onNavigateRecord(item.iri)}
                />
              ))}
            </Stack>
            {beat.code_anchors.length > 0 && (
              <Stack direction="row" spacing={1} useFlexGap flexWrap="wrap" sx={{ mt: 1 }}>
                {beat.code_anchors.map((anchor) => (
                  <Chip
                    key={anchor.symbol}
                    clickable={Boolean(anchor.entity_iri)}
                    size="small"
                    label={`${anchor.label}${anchor.path ? ` · ${anchor.path}${anchor.line != null ? `:${anchor.line}` : ''}` : ''}`}
                    onClick={() => anchor.entity_iri && onNavigateRecord(anchor.entity_iri)}
                  />
                ))}
              </Stack>
            )}
          </Paper>
        ))}
      </Stack>

      {story.gaps.length > 0 && (
        <Alert severity="warning">
          <Typography variant="subtitle2">This Story cannot currently answer everything</Typography>
          {story.gaps.map((gap) => <Typography key={gap.id} variant="body2">{gap.title}: {gap.detail}</Typography>)}
        </Alert>
      )}

      {story.checks.length > 0 && (
        <Paper variant="outlined" sx={{ p: { xs: 2, md: 3 } }}>
          <Typography variant="h6">Check your understanding</Typography>
          <Typography variant="body2" color="text.secondary" sx={{ mb: 2 }}>
            Answers are checked against accepted graph relationships, not generated prose.
          </Typography>
          <Stack spacing={2.5}>
            {story.checks.map((check) => {
              const result = results[check.id];
              const gradeError = gradeErrors[check.id];
              return (
                <Box key={check.id}>
                  <Typography variant="subtitle2">{check.question}</Typography>
                  <RadioGroup value={selected[check.id] ?? ''} onChange={(event) => selectAnswer(check.id, event.target.value)}>
                    {check.options.map((option) => <FormControlLabel key={option.id} value={option.id} control={<Radio size="small" />} label={option.label} />)}
                  </RadioGroup>
                  <Button size="small" disabled={!selected[check.id] || grading[check.id]} onClick={() => grade(check.id)}>Check answer</Button>
                  {result && <Alert severity={result.correct ? 'success' : 'info'} sx={{ mt: 1 }}>{result.feedback}</Alert>}
                  {gradeError && <Alert severity="error" sx={{ mt: 1 }}>{gradeError}</Alert>}
                </Box>
              );
            })}
          </Stack>
        </Paper>
      )}
    </Stack>
  );
}

export default function StoriesPage({ onNavigateRecord, initialComponentIri, onDirtyChange }: StoriesPageProps) {
  const [library, setLibrary] = useState<StoryListResponse | null>(null);
  const [subjectMode, setSubjectMode] = useState<'entity' | 'topic'>('entity');
  const [subjects, setSubjects] = useState<StorySubjectCandidate[]>([]);
  const [subjectsLoading, setSubjectsLoading] = useState(false);
  const [subjectQuery, setSubjectQuery] = useState('');
  const [selectedSubject, setSelectedSubject] = useState<StorySubjectCandidate | null>(null);
  const [topic, setTopic] = useState('');
  const [assistLevel, setAssistLevel] = useState<StoryAssistLevel>(1);
  const [generated, setGenerated] = useState<StoryGenerateResponse | null>(null);
  const [editor, setEditor] = useState<StoryRecipe | null>(null);
  const [editorBaseline, setEditorBaseline] = useState<StoryRecipe | null>(null);
  const [busy, setBusy] = useState(false);
  const [assisting, setAssisting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [warning, setWarning] = useState<string | null>(null);
  const generationRef = useRef(0);
  const generationOperationRef = useRef<number | null>(null);
  const saveGeneratedRef = useRef(false);
  const editorOperationRef = useRef(false);
  const libraryActionRef = useRef(false);
  const libraryRequestRef = useRef(0);
  const subjectCatalogRequestRef = useRef(0);
  const editorDirty = Boolean(editor && editorBaseline && JSON.stringify(editor) !== JSON.stringify(editorBaseline));

  useEffect(() => {
    onDirtyChange?.(editorDirty);
  }, [editorDirty, onDirtyChange]);
  useEffect(
    () => () => {
      onDirtyChange?.(false);
    },
    [onDirtyChange],
  );

  const refresh = () => api.listStories().then(setLibrary);
  const appendWarning = (message: string) =>
    setWarning((current) => (current ? `${current} ${message}` : message));
  const refreshBestEffort = async (completedAction: string) => {
    try {
      await refresh();
    } catch (err) {
      appendWarning(`${completedAction}, but the library could not be refreshed: ${errorMessage(err)}`);
    }
  };
  const reloadStoryReader = async (recipeId: string, completedAction: string) => {
    try {
      const response = await api.generateStory({ recipe_id: recipeId, assist_level: assistLevel });
      setGenerated(response);
    } catch (err) {
      appendWarning(`${completedAction}, but its reader could not be reloaded: ${errorMessage(err)}`);
    }
  };
  useEffect(() => {
    refresh().catch((err) => setError(errorMessage(err)));
  }, []);

  const loadSubjectCatalog = useCallback(async () => {
    const request = ++subjectCatalogRequestRef.current;
    setSubjectsLoading(true);
    try {
      const response = await api.listStorySubjects();
      if (subjectCatalogRequestRef.current === request) setSubjects(response.subjects);
    } catch (err) {
      if (subjectCatalogRequestRef.current === request) setError(errorMessage(err));
    } finally {
      if (subjectCatalogRequestRef.current === request) setSubjectsLoading(false);
    }
  }, []);

  useEffect(() => {
    if (subjectMode === 'entity') void loadSubjectCatalog();
  }, [loadSubjectCatalog, subjectMode]);

  const improveNarration = async (
    request: StorySelectionRequest,
    symbolicStory: StoryRun,
    generation: number,
  ) => {
    setAssisting(true);
    try {
      const assisted = await api.generateStory({
        ...request,
        assist_level: 1,
        include_checks: false,
      });
      if (generationRef.current !== generation) return;

      const upgraded =
        assisted.outcome === 'story'
          ? applyAssistedNarration(symbolicStory, assisted.story)
          : null;
      if (upgraded) {
        setGenerated({ outcome: 'story', story: upgraded });
      } else {
        setWarning('Assisted narration did not match the symbolic Story structure; showing the symbolic Story.');
      }
    } catch (err) {
      if (generationRef.current === generation) {
        setWarning(`Assisted narration was unavailable; showing the symbolic Story: ${errorMessage(err)}`);
      }
    } finally {
      if (generationRef.current === generation) setAssisting(false);
    }
  };

  const generate = async (request: StorySelectionRequest) => {
    if (editor || generationOperationRef.current !== null) return;
    const generation = ++generationRef.current;
    generationOperationRef.current = generation;
    setBusy(true);
    setAssisting(false);
    setError(null);
    setWarning(null);
    setEditor(null);
    setEditorBaseline(null);
    try {
      const symbolic = await api.generateStory({ ...request, assist_level: 0 });
      if (generationOperationRef.current === generation) {
        generationOperationRef.current = null;
      }
      if (generationRef.current !== generation) return;
      setGenerated(symbolic);
      libraryActionRef.current = false;
      setBusy(false);
      if (assistLevel === 1 && symbolic.outcome === 'story') {
        await improveNarration(request, symbolic.story, generation);
      }
    } catch (err) {
      if (generationRef.current === generation) setError(errorMessage(err));
    } finally {
      if (generationOperationRef.current === generation) {
        generationOperationRef.current = null;
      }
      if (generationRef.current === generation) setBusy(false);
    }
  };

  const invalidateGeneration = () => {
    generationRef.current += 1;
    generationOperationRef.current = null;
    setAssisting(false);
  };

  const beginBlockingAction = () => {
    invalidateGeneration();
    setBusy(true);
    setError(null);
    setWarning(null);
  };

  useEffect(() => {
    if (initialComponentIri) {
      setSubjectMode('entity');
      setSelectedSubject({
        iri: initialComponentIri,
        kind: 'SystemComponent',
        label: initialComponentIri,
      });
      generate({ subject_iri: initialComponentIri });
    }
  }, [initialComponentIri]);

  const submitSubject = (event: FormEvent) => {
    event.preventDefault();
    if (subjectMode === 'entity' && selectedSubject) {
      generate({ subject_iri: selectedSubject.iri });
    } else if (subjectMode === 'topic' && topic.trim().length >= 2) {
      generate({ topic: topic.trim() });
    }
  };

  const beginLibraryAction = () => {
    if (libraryActionRef.current) return null;
    libraryActionRef.current = true;
    libraryRequestRef.current += 1;
    return libraryRequestRef.current;
  };
  const finishLibraryAction = (request: number) => {
    if (libraryRequestRef.current === request) libraryActionRef.current = false;
  };
  const openSummary = async (story: StorySummary) => {
    const request = beginLibraryAction();
    if (request == null) return;
    try {
      await generate({ recipe_id: story.id });
    } finally {
      finishLibraryAction(request);
    }
  };
  const editSummary = async (story: StorySummary) => {
    const request = beginLibraryAction();
    if (request == null) return;
    beginBlockingAction();
    try {
      const response = await api.getStory(story.id);
      if (libraryRequestRef.current === request) {
        setEditor(response.recipe);
        setEditorBaseline(response.recipe);
      }
    } catch (err) {
      if (libraryRequestRef.current === request) {
        setError(errorMessage(err));
      }
    } finally {
      if (libraryRequestRef.current === request) setBusy(false);
      finishLibraryAction(request);
    }
  };

  const saveRecipe = async () => {
    if (!editor || editorOperationRef.current) return;
    editorOperationRef.current = true;
    beginBlockingAction();
    try {
      const response = await api.saveStory(editor);
      setEditor(response.recipe);
      setEditorBaseline(response.recipe);
      setGenerated(null);
      await reloadStoryReader(response.recipe.id, 'Story was saved');
      await refreshBestEffort('Story was saved');
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      editorOperationRef.current = false;
      setBusy(false);
    }
  };

  const publishRecipe = async () => {
    if (!editor || editorOperationRef.current) return;
    editorOperationRef.current = true;
    beginBlockingAction();
    try {
      const saved = await api.saveStory(editor);
      setEditor(saved.recipe);
      setEditorBaseline(saved.recipe);
      setGenerated(null);
      if (!saved.recipe.updated_at) {
        setError('Story changes were saved, but the server did not return the updated_at token required to publish');
        await refreshBestEffort('Story changes were saved');
        return;
      }
      let response;
      try {
        response = await api.publishStory(saved.recipe.id, saved.recipe.updated_at);
      } catch (err) {
        setError(`Story changes were saved, but publication failed: ${errorMessage(err)}`);
        await refreshBestEffort('Story changes were saved');
        return;
      }
      setEditor(response.recipe);
      setEditorBaseline(response.recipe);
      await reloadStoryReader(response.recipe.id, 'Story was published');
      await refreshBestEffort('Story was published');
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      editorOperationRef.current = false;
      setBusy(false);
    }
  };

  const currentStory = generated?.outcome === 'story' ? generated.story : null;
  const saveGenerated = async () => {
    if (!currentStory || saveGeneratedRef.current) return;
    saveGeneratedRef.current = true;
    beginBlockingAction();
    try {
      const response = await api.saveStory(recipeFromRun(currentStory));
      if (!response.recipe.id) {
        throw new Error('Saved Story did not return the recipe ID required to reload it');
      }
      setGenerated({
        outcome: 'story',
        story: {
          ...currentStory,
          recipe_id: response.recipe.id,
          trust_state: response.recipe.status,
        },
      });
      await reloadStoryReader(response.recipe.id, 'Story was saved as draft');
      await refreshBestEffort('Story was saved as draft');
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      saveGeneratedRef.current = false;
      setBusy(false);
    }
  };

  const curateCurrent = async () => {
    if (!currentStory?.recipe_id) return;
    await editSummary({ id: currentStory.recipe_id } as StorySummary);
  };

  return (
    <Box sx={{ height: '100%', overflow: 'auto', p: { xs: 2, md: 3 }, bgcolor: 'background.default' }}>
      <Stack spacing={3} sx={{ maxWidth: 1250, mx: 'auto' }}>
        <Box>
          <Stack direction="row" spacing={1} alignItems="center"><AutoStoriesIcon color="primary" /><Typography variant="h4">Stories</Typography></Stack>
          <Typography variant="body2" color="text.secondary" sx={{ mt: 0.75 }}>
            Build a short, evidence-backed path through a project entity or topic: why it matters, how it connects, and what to understand before making changes.
          </Typography>
        </Box>

        <Paper component="form" onSubmit={submitSubject} variant="outlined" sx={{ p: 2 }}>
          <Stack spacing={1.5}>
            <Tabs
              value={subjectMode}
              onChange={(_event, value: 'entity' | 'topic') => setSubjectMode(value)}
              aria-label="Story subject mode"
            >
              <Tab value="entity" label="Entity" disabled={Boolean(editor)} />
              <Tab value="topic" label="Topic" disabled={Boolean(editor)} />
            </Tabs>
            <Stack direction={{ xs: 'column', md: 'row' }} spacing={1.5} alignItems={{ md: 'flex-start' }}>
              {subjectMode === 'entity' ? (
                <Autocomplete
                  fullWidth
                  disabled={Boolean(editor)}
                  loading={subjectsLoading}
                  options={subjects}
                  value={selectedSubject}
                  inputValue={subjectQuery}
                  filterOptions={(options, state) => filterStorySubjects(options, state.inputValue)}
                  groupBy={(option) => option.kind}
                  getOptionLabel={(option) => option.label}
                  isOptionEqualToValue={(option, value) => option.iri === value.iri}
                  onOpen={() => {
                    setSubjectQuery('');
                    void loadSubjectCatalog();
                  }}
                  onClose={() => {
                    if (selectedSubject) setSubjectQuery(selectedSubject.label);
                  }}
                  onInputChange={(_event, value, reason) => {
                    if (reason === 'input' || reason === 'clear') {
                      setSubjectQuery(value);
                      setSelectedSubject(null);
                    }
                  }}
                  onChange={(_event, value) => {
                    setSelectedSubject(value);
                    setSubjectQuery(value?.label ?? '');
                  }}
                  renderInput={(params) => (
                    <TextField
                      {...params}
                      label="Find an entity"
                      placeholder="Component, decision, requirement, lesson, or code symbol"
                      helperText="Browse the complete current catalog by category, or type to filter entities, records, and code."
                    />
                  )}
                  renderOption={(props, option) => (
                    <li {...props} key={option.iri}>
                      <Box>
                        <Typography variant="body2">{option.label}</Typography>
                        {option.description ? (
                          <Typography variant="caption" color="text.secondary">
                            {option.description}
                          </Typography>
                        ) : null}
                      </Box>
                    </li>
                  )}
                />
              ) : (
                <TextField
                  fullWidth
                  disabled={Boolean(editor)}
                  label="Topic"
                  placeholder="For example: why Story generation is symbolic-first"
                  value={topic}
                  onChange={(event) => setTopic(event.target.value)}
                  helperText="MOOSEDev retrieves a bounded set of current project records; it does not create a Topic node in the graph."
                />
              )}
              <FormControl disabled={Boolean(editor)} sx={{ minWidth: 220 }}>
                <InputLabel>Narration</InputLabel>
                <Select label="Narration" value={assistLevel} onChange={(event) => setAssistLevel(Number(event.target.value) as StoryAssistLevel)}>
                  <MenuItem value={0}>Symbolic summary</MenuItem>
                  <MenuItem value={1}>Plain-language LLM assist</MenuItem>
                </Select>
              </FormControl>
              <Button
                type="submit"
                variant="contained"
                disabled={
                  busy ||
                  Boolean(editor) ||
                  (subjectMode === 'entity' ? !selectedSubject : topic.trim().length < 2)
                }
                sx={{ minWidth: 120, minHeight: 56 }}
              >
                {busy ? <CircularProgress size={20} /> : 'Tell Story'}
              </Button>
            </Stack>
          </Stack>
        </Paper>

        {error && <Alert severity="error">{error}</Alert>}
        {warning && <Alert severity="warning">{warning}</Alert>}

        {generated?.outcome === 'ambiguous' && (
          <Alert severity="info" icon={<AutoStoriesIcon />}>
            <Typography variant="subtitle2">Which subject did you mean?</Typography>
            <Stack direction="row" spacing={1} useFlexGap flexWrap="wrap" sx={{ mt: 1 }}>
              {generated.candidates.map((candidate) => (
                <Button
                  key={candidate.iri}
                  size="small"
                  variant="outlined"
                  disabled={busy}
                  onClick={() =>
                    generate({
                      prompt: generated.prompt,
                      ...(generated.recipe_id ? { recipe_id: generated.recipe_id } : {}),
                      subject_iri: candidate.iri,
                    })
                  }
                >
                  {candidate.label}
                </Button>
              ))}
            </Stack>
          </Alert>
        )}

        {editor ? (
          <StoryEditor
            recipe={editor}
            busy={busy}
            dirty={editorDirty}
            onChange={setEditor}
            onSave={saveRecipe}
            onPublish={publishRecipe}
            onClose={() => {
              invalidateGeneration();
              setEditor(null);
              setEditorBaseline(null);
            }}
          />
        ) : currentStory ? (
          <StoryReader
            story={currentStory}
            onNavigateRecord={onNavigateRecord}
            onSaveDraft={saveGenerated}
            onCurate={curateCurrent}
            onClose={() => {
              invalidateGeneration();
              setGenerated(null);
            }}
            onGenerateFresh={() => generate({ ...storySelection(currentStory.subject), fresh: true })}
            busy={busy}
            assisting={assisting}
          />
        ) : (
          <Box sx={{ display: 'grid', gridTemplateColumns: { xs: '1fr', md: 'minmax(0, 1fr) 340px' }, gap: 3 }}>
            <Paper variant="outlined" sx={{ p: 3 }}>
              <Typography variant="h6">Start with a project entity or a focused topic</Typography>
              <Typography variant="body2" color="text.secondary" sx={{ mt: 1, maxWidth: 650 }}>
                Choose a current component, knowledge record, or code entity—or ask about a focused topic. MOOSEDev selects three to five beats from current project knowledge; missing rationale stays visible as a gap, and narration never becomes authoritative knowledge.
              </Typography>
            </Paper>
            <Box>
              <Typography variant="h6" sx={{ mb: 1.5 }}>Saved Stories</Typography>
              {library ? <StoryLibrary data={library} onOpen={openSummary} onEdit={editSummary} busy={busy} /> : <CircularProgress size={20} />}
            </Box>
          </Box>
        )}
      </Stack>
    </Box>
  );
}
