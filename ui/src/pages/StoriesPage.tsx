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

/**
 * The entity IRI a generated response puts on screen, or null when it is a
 * topic (which has no canonical route) or not a Story at all.
 */
function displayedSubjectIri(response: StoryGenerateResponse): string | null {
  if (response.outcome !== 'story') return null;
  const subject = response.story.subject;
  return subject.type === 'entity' ? subject.iri : null;
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
  // Which generation currently owns the assisted-narration indicator.
  const assistingGenerationRef = useRef<number | null>(null);
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
    assistingGenerationRef.current = generation;
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
      // Clear only if THIS assist still owns the indicator. Guarding on the
      // generation token alone left the chip up forever once the token moved;
      // clearing unconditionally let a late straggler hide a newer assist's
      // progress. Ownership answers both.
      if (assistingGenerationRef.current === generation) {
        assistingGenerationRef.current = null;
        setAssisting(false);
      }
    }
  };

  const reloadStoryReader = async (
    recipeId: string,
    completedAction: string,
    generation: number,
  ) => {
    try {
      const symbolic = await api.generateStory({ recipe_id: recipeId, assist_level: 0 });
      if (!stillOwns(generation)) return;
      setGenerated(symbolic);
      // Curating straight from the library starts with no Story hash, so
      // without this the URL keeps naming nothing while a Story is on screen —
      // and refreshing lands back on the default page. Deep links are meant to
      // be refreshable, so whatever is displayed must name itself.
      onSubjectChange?.(displayedSubjectIri(symbolic));
      if (assistLevel === 1 && symbolic.outcome === 'story') {
        void improveNarration({ recipe_id: recipeId }, symbolic.story, generation);
      }
    } catch (err) {
      if (stillOwns(generation)) {
        appendWarning(
          `${completedAction}, but its reader could not be reloaded: ${errorMessage(err)}`,
        );
      }
    }
  };

  const generate = async (request: StorySelectionRequest, replacesCurrent = false) => {
    // A deep link has already been confirmed by the navigation guard, so it
    // SUPERSEDES whatever is open OR still in flight — bumping the generation
    // token below is what makes the older response discard itself. Deferring
    // here instead would strand the older Story on screen under the new URL.
    // Every other caller still defers, rather than yanking the page out from
    // under a curator or stacking duplicate work.
    if (!replacesCurrent && (editor || generationOperationRef.current !== null)) return;
    const generation = ++generationRef.current;
    generationOperationRef.current = generation;
    setBusy(true);
    assistingGenerationRef.current = null;
    setAssisting(false);
    setError(null);
    setWarning(null);
    try {
      const symbolic = await api.generateStory({ ...request, assist_level: 0 });
      if (generationOperationRef.current === generation) generationOperationRef.current = null;
      if (generationRef.current !== generation) return;
      setGenerated(symbolic);
      onSubjectChange?.(displayedSubjectIri(symbolic));
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

  // Returns the generation the caller now owns. Every async Story operation
  // must carry it and re-check before touching state: a deep link arriving
  // mid-flight bumps the token, and an operation that ignored it would land its
  // `setEditor`/`setGenerated` on top of the Story the URL now names.
  const invalidateGeneration = () => {
    generationRef.current += 1;
    generationOperationRef.current = null;
    assistingGenerationRef.current = null;
    setAssisting(false);
    // The disowned operation can no longer clear this itself — its `finally`
    // now checks ownership. If nothing replaces it (a deep link whose subject
    // lookup fails), the form and library would stay disabled forever.
    // Callers that go on to start work set it straight back.
    setBusy(false);
    return generationRef.current;
  };
  const beginBlockingAction = () => {
    const generation = invalidateGeneration();
    setBusy(true);
    setError(null);
    setWarning(null);
    return generation;
  };
  const stillOwns = (generation: number) => generationRef.current === generation;

  // A deep link whose subject is still resolving must not leave the PREVIOUS
  // Story on screen: the URL already names the new subject, and if the lookup
  // fails that mismatch would otherwise persist indefinitely.
  useEffect(() => {
    if (!subjectResolving) return;
    invalidateGeneration();
    setEditor(null);
    setEditorBaseline(null);
    setGenerated(null);
    // The selector must not keep naming the OUTGOING subject. The catalog is
    // bounded, so an arriving subject may never appear in it to overwrite
    // these — leaving the form ready to regenerate the wrong Story.
    setSelectedSubject(null);
    setSubjectQuery('');
  }, [subjectResolving]);

  useEffect(() => {
    if (!initialSubjectIri) return;
    // The workbench echoes the displayed subject back when it syncs the URL, so
    // this effect re-enters naming the Story already on screen. Regenerating
    // then would throw away that Story along with the reader's progress and
    // graded answers, so only a subject that is NOT displayed is acted on.
    if (generated && displayedSubjectIri(generated) === initialSubjectIri) return;
    setSubjectMode('entity');
    // The navigation to this subject was already accepted (App confirmed any
    // unsaved curation), so the editor and the outgoing Story must not survive
    // it — otherwise the URL names one subject while another stays on screen.
    setEditor(null);
    setEditorBaseline(null);
    setGenerated(null);
    void generate({ subject_iri: initialSubjectIri }, true);
  }, [initialSubjectIri]);

  // Name the deep-linked subject in the selector once the catalog can. Until
  // then the selector stays empty rather than showing a guessed kind or an
  // IRI as if it were a label.
  useEffect(() => {
    if (!initialSubjectIri) return;
    const match = subjects.find((subject) => subject.iri === initialSubjectIri);
    if (match) {
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
    const generation = beginBlockingAction();
    try {
      const response = await api.getStory(storyId);
      if (!stillOwns(generation)) return;
      setEditor(response.recipe);
      setEditorBaseline(response.recipe);
    } catch (err) {
      if (stillOwns(generation)) setError(errorMessage(err));
    } finally {
      if (stillOwns(generation)) setBusy(false);
      libraryActionRef.current = false;
    }
  };

  const saveRecipe = async () => {
    if (!editor || editorOperationRef.current) return;
    editorOperationRef.current = true;
    const generation = beginBlockingAction();
    try {
      const response = await api.saveStory(editor);
      if (!stillOwns(generation)) return;
      setEditor(response.recipe);
      setEditorBaseline(response.recipe);
      setGenerated(null);
      await reloadStoryReader(response.recipe.id, 'Story was saved', generation);
      await refreshBestEffort('Story was saved');
    } catch (err) {
      if (stillOwns(generation)) setError(errorMessage(err));
    } finally {
      editorOperationRef.current = false;
      if (stillOwns(generation)) setBusy(false);
    }
  };

  const publishRecipe = async () => {
    if (!editor || editorOperationRef.current) return;
    editorOperationRef.current = true;
    const generation = beginBlockingAction();
    try {
      const saved = await api.saveStory(editor);
      if (!stillOwns(generation)) return;
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
        if (!stillOwns(generation)) return;
        setEditor(response.recipe);
        setEditorBaseline(response.recipe);
        await reloadStoryReader(response.recipe.id, 'Story was published', generation);
        await refreshBestEffort('Story was published');
      } catch (err) {
        if (!stillOwns(generation)) return;
        setError(`Story changes were saved, but publication failed: ${errorMessage(err)}`);
        await refreshBestEffort('Story changes were saved');
      }
    } catch (err) {
      if (stillOwns(generation)) setError(errorMessage(err));
    } finally {
      editorOperationRef.current = false;
      if (stillOwns(generation)) setBusy(false);
    }
  };

  const currentStory = generated?.outcome === 'story' ? generated.story : null;
  const saveGenerated = async () => {
    if (!currentStory || saveGeneratedRef.current) return;
    saveGeneratedRef.current = true;
    const generation = beginBlockingAction();
    try {
      const response = await api.saveStory(recipeFromRun(currentStory));
      if (!stillOwns(generation)) return;
      setGenerated({
        outcome: 'story',
        story: {
          ...currentStory,
          recipe_id: response.recipe.id,
          trust_state: response.recipe.status,
        },
      });
      await reloadStoryReader(response.recipe.id, 'Story was saved as draft', generation);
      await refreshBestEffort('Story was saved as draft');
    } catch (err) {
      if (stillOwns(generation)) setError(errorMessage(err));
    } finally {
      saveGeneratedRef.current = false;
      if (stillOwns(generation)) setBusy(false);
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
              onSubjectChange?.(null);
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
