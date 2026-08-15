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
  StoryGenerateResponse,
  StoryListResponse,
  StoryRecipe,
  StoryRun,
  StoryStatus,
  StorySubjectCandidate,
} from '../api/types';
import StoryEditor from './stories/StoryEditor';
import StoryReader, { TrustBadge } from './stories/StoryReader';
import {
  applyAssistedNarration,
  errorMessage,
  filterStorySubjects,
  recipeFromRun,
  storySelection,
  StorySelectionRequest,
} from './stories/storyModel';

export { applyAssistedNarration } from './stories/storyModel';

interface StoriesPageProps {
  onNavigateRecord: (iri: string) => void;
  initialComponentIri?: string | null;
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
  initialComponentIri,
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
  const [generated, setGenerated] = useState<StoryGenerateResponse | null>(null);
  const [editor, setEditor] = useState<StoryRecipe | null>(null);
  const [editorBaseline, setEditorBaseline] = useState<StoryRecipe | null>(null);
  const [busy, setBusy] = useState(false);
  const [assisting, setAssisting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [warning, setWarning] = useState<string | null>(null);

  // Tokens reject stale LLM results; boolean refs synchronously guard clicks before `busy` renders.
  const generationRef = useRef(0);
  const generationOperationRef = useRef<number | null>(null);
  const saveGeneratedRef = useRef(false);
  const editorOperationRef = useRef(false);
  const libraryActionRef = useRef(false);
  const subjectCatalogRequestRef = useRef(0);
  const editorDirty = Boolean(
    editor && editorBaseline && JSON.stringify(editor) !== JSON.stringify(editorBaseline),
  );

  useEffect(() => { onDirtyChange?.(editorDirty); }, [editorDirty, onDirtyChange]);
  useEffect(() => () => { onDirtyChange?.(false); }, [onDirtyChange]);

  const refresh = () => api.listStories().then(setLibrary);
  const appendWarning = (message: string) => {
    setWarning((current) => current ? `${current} ${message}` : message);
  };
  const refreshBestEffort = async (completedAction: string) => {
    try {
      await refresh();
    } catch (err) {
      appendWarning(
        `${completedAction}, but the library could not be refreshed: ${errorMessage(err)}`,
      );
    }
  };
  useEffect(() => { refresh().catch((err) => setError(errorMessage(err))); }, []);

  const loadSubjectCatalog = useCallback(async () => {
    const request = ++subjectCatalogRequestRef.current;
    setSubjectsLoading(true);
    try {
      const response = await api.listStorySubjects(undefined, 5_000);
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
      const upgraded = assisted.outcome === 'story'
        ? applyAssistedNarration(symbolicStory, assisted.story)
        : null;
      if (upgraded) {
        setGenerated({ outcome: 'story', story: upgraded });
      } else {
        setWarning(
          'Assisted narration did not match the symbolic Story structure; showing the symbolic Story.',
        );
      }
    } catch (err) {
      if (generationRef.current === generation) {
        setWarning(
          `Assisted narration was unavailable; showing the symbolic Story: ${errorMessage(err)}`,
        );
      }
    } finally {
      if (generationRef.current === generation) setAssisting(false);
    }
  };

  const reloadStoryReader = async (recipeId: string, completedAction: string) => {
    try {
      const symbolic = await api.generateStory({ recipe_id: recipeId, assist_level: 0 });
      setGenerated(symbolic);
      if (assistLevel === 1 && symbolic.outcome === 'story') {
        void improveNarration({ recipe_id: recipeId }, symbolic.story, generationRef.current);
      }
    } catch (err) {
      appendWarning(
        `${completedAction}, but its reader could not be reloaded: ${errorMessage(err)}`,
      );
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
    try {
      const symbolic = await api.generateStory({ ...request, assist_level: 0 });
      if (generationOperationRef.current === generation) generationOperationRef.current = null;
      if (generationRef.current !== generation) return;
      setGenerated(symbolic);
      setBusy(false);
      if (assistLevel === 1 && symbolic.outcome === 'story') {
        await improveNarration(request, symbolic.story, generation);
      }
    } catch (err) {
      if (generationRef.current === generation) setError(errorMessage(err));
    } finally {
      if (generationOperationRef.current === generation) generationOperationRef.current = null;
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
    if (!initialComponentIri) return;
    setSubjectMode('entity');
    setSelectedSubject({
      iri: initialComponentIri,
      kind: 'SystemComponent',
      label: initialComponentIri,
    });
    void generate({ subject_iri: initialComponentIri });
  }, [initialComponentIri]);

  const submitSubject = (event: FormEvent) => {
    event.preventDefault();
    if (subjectMode === 'entity' && selectedSubject) {
      void generate({ subject_iri: selectedSubject.iri });
    } else if (subjectMode === 'topic' && topic.trim().length >= 2) {
      void generate({ topic: topic.trim() });
    }
  };

  const beginLibraryAction = () => {
    if (libraryActionRef.current) return false;
    libraryActionRef.current = true;
    return true;
  };
  const openStory = async (storyId: string) => {
    if (!beginLibraryAction()) return;
    try {
      await generate({ recipe_id: storyId });
    } finally {
      libraryActionRef.current = false;
    }
  };
  const editStory = async (storyId: string) => {
    if (!beginLibraryAction()) return;
    beginBlockingAction();
    try {
      const response = await api.getStory(storyId);
      setEditor(response.recipe);
      setEditorBaseline(response.recipe);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
      libraryActionRef.current = false;
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
        setError(
          'Story changes were saved, but the server did not return the updated_at token required to publish',
        );
        await refreshBestEffort('Story changes were saved');
        return;
      }
      try {
        const response = await api.publishStory(saved.recipe.id, saved.recipe.updated_at);
        setEditor(response.recipe);
        setEditorBaseline(response.recipe);
        await reloadStoryReader(response.recipe.id, 'Story was published');
        await refreshBestEffort('Story was published');
      } catch (err) {
        setError(`Story changes were saved, but publication failed: ${errorMessage(err)}`);
        await refreshBestEffort('Story changes were saved');
      }
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

  const closeEditor = () => {
    invalidateGeneration();
    setEditor(null);
    setEditorBaseline(null);
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
                      helperText="Browse the complete current catalog by category, or type to filter."
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
            onClose={() => {
              invalidateGeneration();
              setGenerated(null);
            }}
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
