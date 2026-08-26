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
  InputLabel,
  MenuItem,
  Paper,
  Select,
  Stack,
  Tab,
  Tabs,
  TextField,
  Typography,
} from '@mui/material';
import AutoStoriesIcon from '@mui/icons-material/AutoStories';
import EditOutlinedIcon from '@mui/icons-material/EditOutlined';
import { api } from '../api/client';
import {
  StoryAssistLevel,
  StoryListResponse,
  StoryStatus,
  StorySubjectCandidate,
} from '../api/types';
import StoryEditor from './stories/StoryEditor';
import StoryReader, { TrustBadge } from './stories/StoryReader';
import {
  filterStorySubjects,
  storySelection,
} from './stories/storyModel';
import { useStoryGeneration } from './stories/useStoryGeneration';

export { applyAssistedNarration } from './stories/storyModel';

interface StoriesPageProps {
  onNavigateRecord: (iri: string) => void;
  /** Any current Story subject IRI — a component, a record, or a CodeEntity. */
  initialSubjectIri?: string | null;
  /** True while a deep link's subject is still being resolved by the daemon. */
  subjectResolving?: boolean;
  /** The subject now on screen, so the URL can stop naming a stale one. */
  onSubjectChange?: (subjectIri: string | null) => void;
  onDirtyChange?: (dirty: boolean) => void;
}

interface StoryLibraryProps {
  data: StoryListResponse;
  onOpen: (storyId: string) => void;
  onEdit: (storyId: string) => void;
  busy: boolean;
}

function StoryLibrary({ data, onOpen, onEdit, busy }: StoryLibraryProps) {
  const groups = useMemo(() => ({
    published: data.stories.filter((story) => story.status === 'published'),
    draft: data.stories.filter((story) => story.status === 'draft'),
  }), [data]);
  const statuses: StoryStatus[] = ['published', 'draft'];

  if (data.stories.length === 0) {
    return <Typography variant="body2" color="text.secondary">No saved Stories yet.</Typography>;
  }
  return (
    <Stack spacing={2.5}>
      {statuses.map((status) => groups[status].length ? (
        <Box key={status}>
          <Typography variant="overline" color="text.secondary">{status}</Typography>
          <Stack spacing={1}>
            {groups[status].map((story) => (
              <Card key={story.id} variant="outlined">
                <CardActionArea disabled={busy} onClick={() => onOpen(story.id)}>
                  <CardContent sx={{ pb: 1.5 }}>
                    <Stack
                      direction="row"
                      spacing={1}
                      justifyContent="space-between"
                      alignItems="flex-start"
                    >
                      <Typography variant="subtitle2">{story.title}</Typography>
                      <TrustBadge state={story.status} />
                    </Stack>
                    <Typography
                      variant="caption"
                      color="text.secondary"
                      sx={{ display: 'block', mt: 0.75 }}
                    >
                      {story.subject_kind}: {story.subject_label}
                    </Typography>
                    {story.drifted ? (
                      <ChipWarning />
                    ) : null}
                  </CardContent>
                </CardActionArea>
                <Divider />
                <Button
                  disabled={busy}
                  size="small"
                  startIcon={<EditOutlinedIcon />}
                  onClick={() => onEdit(story.id)}
                  sx={{ m: 0.5 }}
                >
                  Curate
                </Button>
              </Card>
            ))}
          </Stack>
        </Box>
      ) : null)}
    </Stack>
  );
}

function ChipWarning() {
  return (
    <Chip
      size="small"
      color="warning"
      variant="outlined"
      label="Changed since curation"
      sx={{ mt: 1 }}
    />
  );
}

export default function StoriesPage({
  onNavigateRecord,
  initialSubjectIri,
  subjectResolving = false,
  onSubjectChange,
  onDirtyChange,
}: StoriesPageProps) {
  const [library, setLibrary] = useState<StoryListResponse | null>(null);
  const [subjectMode, setSubjectMode] = useState<'entity' | 'topic'>('entity');
  const [subjects, setSubjects] = useState<StorySubjectCandidate[]>([]);
  const [subjectsLoading, setSubjectsLoading] = useState(false);
  const [subjectQuery, setSubjectQuery] = useState('');
  const [selectedSubject, setSelectedSubject] = useState<StorySubjectCandidate | null>(null);
  const [topic, setTopic] = useState('');
  const [assistLevel, setAssistLevel] = useState<StoryAssistLevel>(1);
  const subjectCatalogRequestRef = useRef(0);
  // Which subject the selector has already been named for, so a catalog
  // refresh never re-asserts it over the reader's own choice.
  const namedSubjectRef = useRef<string | null>(null);
  const refreshLibrary = useCallback(() => api.listStories().then(setLibrary), []);
  const {
    assisting,
    busy,
    closeEditor,
    closeReader,
    currentStory,
    editStory,
    editor,
    editorDirty,
    error,
    generate,
    generated,
    openStory,
    publishRecipe,
    replaceWith,
    reportError,
    resetForNavigation,
    saveGenerated,
    saveRecipe,
    setEditor,
    warning,
  } = useStoryGeneration({
    assistLevel,
    onDirtyChange,
    onSubjectChange,
    refreshLibrary,
  });

  const loadSubjectCatalog = useCallback(async () => {
    const request = ++subjectCatalogRequestRef.current;
    setSubjectsLoading(true);
    try {
      const response = await api.listStorySubjects(undefined, 5_000);
      if (subjectCatalogRequestRef.current === request) setSubjects(response.subjects);
    } catch (err) {
      if (subjectCatalogRequestRef.current === request) reportError(err);
    } finally {
      if (subjectCatalogRequestRef.current === request) setSubjectsLoading(false);
    }
  }, []);

  useEffect(() => {
    if (subjectMode === 'entity') void loadSubjectCatalog();
  }, [loadSubjectCatalog, subjectMode]);

  // A deep link whose subject is still resolving must not leave the PREVIOUS
  // Story on screen: the URL already names the new subject, and if the lookup
  // fails that mismatch would otherwise persist indefinitely.
  useEffect(() => {
    if (!subjectResolving) return;
    resetForNavigation();
    // The selector must not keep naming the OUTGOING subject. The catalog is
    // bounded, so an arriving subject may never appear in it to overwrite
    // these — leaving the form ready to regenerate the wrong Story.
    setSelectedSubject(null);
    setSubjectQuery('');
    // The selector was cleared on purpose, so the arriving subject has not been
    // named yet and must be allowed to name it again.
    namedSubjectRef.current = null;
  }, [subjectResolving]);

  useEffect(() => {
    if (!initialSubjectIri) return;
    setSubjectMode('entity');
    replaceWith(initialSubjectIri);
  }, [initialSubjectIri]);

  // Name the deep-linked subject in the selector once the catalog can. Until
  // then the selector stays empty rather than showing a guessed kind or an
  // IRI as if it were a label.
  useEffect(() => {
    // Name each subject at most ONCE. The catalog is refetched on every open
    // (Lesson 7039c7f3), so this effect re-runs and would overwrite the reader's
    // choice — a URL-named subject made the dropdown impossible to change.
    // "The selector is empty" is not a usable guard: typing clears
    // `selectedSubject`, so searching would re-arm the assignment.
    if (!initialSubjectIri || namedSubjectRef.current === initialSubjectIri) return;
    const match = subjects.find((subject) => subject.iri === initialSubjectIri);
    if (match) {
      namedSubjectRef.current = initialSubjectIri;
      setSelectedSubject(match);
      setSubjectQuery(match.label);
    }
  }, [initialSubjectIri, subjects]);

  const submitSubject = (event: FormEvent) => {
    event.preventDefault();
    if (subjectMode === 'entity' && selectedSubject) {
      void generate({ subject_iri: selectedSubject.iri });
    } else if (subjectMode === 'topic' && topic.trim().length >= 2) {
      void generate({ topic: topic.trim() });
    }
  };

  return (
    <Box
      sx={{ height: '100%', overflow: 'auto', p: { xs: 2, md: 3 }, bgcolor: 'background.default' }}
    >
      <Stack spacing={3} sx={{ maxWidth: 1250, mx: 'auto' }}>
        <Box>
          <Stack direction="row" spacing={1} alignItems="center">
            <AutoStoriesIcon color="primary" />
            <Typography variant="h4">Stories</Typography>
          </Stack>
          <Typography variant="body2" color="text.secondary" sx={{ mt: 0.75 }}>
            Recover how a project entity or topic came to be, what it means now, and which evidence supports the account.
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
            <Stack
              direction={{ xs: 'column', md: 'row' }}
              spacing={1.5}
              alignItems={{ md: 'flex-start' }}
            >
              {subjectMode === 'entity' ? (
                <Autocomplete
                  fullWidth
                  disabled={Boolean(editor)}
                  loading={subjectsLoading}
                  options={subjects}
                  value={selectedSubject}
                  inputValue={subjectQuery}
                  filterOptions={(options, state) =>
                    filterStorySubjects(options, state.inputValue, selectedSubject)
                  }
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
                      helperText="Browse the subjects MOOSEDev has recorded knowledge about, or type to search every indexed name."
                    />
                  )}
                  renderOption={(props, option) => (
                    <li {...props} key={option.iri}>
                      <Box>
                        <Typography variant="body2">{option.label}</Typography>
                        {option.description ? (
                          <Typography variant="caption" color="text.secondary" display="block">
                            {option.description}
                          </Typography>
                        ) : null}
                        {option.no_recorded_knowledge ? (
                          <Typography variant="caption" color="warning.main" display="block">
                            Nothing recorded about this yet
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
                  helperText="A topic retrieves related project records without creating a Topic node."
                />
              )}
              <FormControl disabled={Boolean(editor)} sx={{ minWidth: 220 }}>
                <InputLabel>Narration</InputLabel>
                <Select
                  label="Narration"
                  value={assistLevel}
                  onChange={(event) => setAssistLevel(Number(event.target.value) as StoryAssistLevel)}
                >
                  <MenuItem value={0}>Symbolic narrative</MenuItem>
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

        {error ? <Alert severity="error">{error}</Alert> : null}
        {warning ? <Alert severity="warning">{warning}</Alert> : null}
        {generated?.outcome === 'ambiguous' ? (
          <Alert severity="info" icon={<AutoStoriesIcon />}>
            <Typography variant="subtitle2">Which subject did you mean?</Typography>
            <Stack direction="row" spacing={1} useFlexGap flexWrap="wrap" sx={{ mt: 1 }}>
              {generated.candidates.map((candidate) => (
                <Button
                  key={candidate.iri}
                  size="small"
                  variant="outlined"
                  disabled={busy}
                  onClick={() => generate({
                    prompt: generated.prompt,
                    ...(generated.recipe_id ? { recipe_id: generated.recipe_id } : {}),
                    subject_iri: candidate.iri,
                  })}
                >
                  {candidate.label}
                </Button>
              ))}
            </Stack>
          </Alert>
        ) : null}

        {editor ? (
          <StoryEditor
            recipe={editor}
            busy={busy}
            dirty={editorDirty}
            onChange={setEditor}
            onSave={saveRecipe}
            onPublish={publishRecipe}
            onClose={closeEditor}
          />
        ) : currentStory ? (
          <StoryReader
            story={currentStory}
            onNavigateRecord={onNavigateRecord}
            onSaveDraft={saveGenerated}
            onCurate={() => {
              if (currentStory.recipe_id) void editStory(currentStory.recipe_id);
            }}
            onClose={closeReader}
            onGenerateFresh={() => generate({
              ...storySelection(currentStory.subject),
              fresh: true,
            })}
            busy={busy}
            assisting={assisting}
          />
        ) : (
          <Box
            sx={{
              display: 'grid',
              gridTemplateColumns: { xs: '1fr', md: 'minmax(0, 1fr) 340px' },
              gap: 3,
            }}
          >
            <Paper variant="outlined" sx={{ p: 3 }}>
              <Typography variant="h6">Start with a project entity or focused topic</Typography>
              <Typography variant="body2" color="text.secondary" sx={{ mt: 1, maxWidth: 650 }}>
                MOOSEDev follows the entity’s typed relationships and history, then presents one
                cohesive narrative with citations, chronology, and explicit knowledge gaps.
              </Typography>
            </Paper>
            <Box>
              <Typography variant="h6" sx={{ mb: 1.5 }}>Saved Stories</Typography>
              {library ? (
                <StoryLibrary
                  data={library}
                  onOpen={openStory}
                  onEdit={editStory}
                  busy={busy}
                />
              ) : (
                <CircularProgress size={20} />
              )}
            </Box>
          </Box>
        )}
      </Stack>
    </Box>
  );
}
