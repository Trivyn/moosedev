import { useEffect, useRef, useState } from 'react';
import { api } from '../../api/client';
import {
  StoryAssistLevel,
  StoryGenerateResponse,
  StoryRecipe,
  StoryRun,
} from '../../api/types';
import {
  applyAssistedNarration,
  errorMessage,
  recipeFromRun,
  StorySelectionRequest,
} from './storyModel';

interface UseStoryGenerationOptions {
  assistLevel: StoryAssistLevel;
  onDirtyChange?: (dirty: boolean) => void;
  onSubjectChange?: (subjectIri: string | null) => void;
  refreshLibrary: () => Promise<void>;
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

export function useStoryGeneration({
  assistLevel,
  onDirtyChange,
  onSubjectChange,
  refreshLibrary,
}: UseStoryGenerationOptions) {
  const [generated, setGenerated] = useState<StoryGenerateResponse | null>(null);
  const [editor, setEditor] = useState<StoryRecipe | null>(null);
  const [editorBaseline, setEditorBaseline] = useState<StoryRecipe | null>(null);
  const [busy, setBusy] = useState(false);
  const [assisting, setAssisting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [warning, setWarning] = useState<string | null>(null);

  // The refs are the synchronous ownership layer behind the rendered status.
  // They reject stale completions and duplicate clicks before React re-renders.
  const generationRef = useRef(0);
  const generationOperationRef = useRef<number | null>(null);
  const assistingGenerationRef = useRef<number | null>(null);
  const saveGeneratedRef = useRef(false);
  const editorOperationRef = useRef(false);
  const libraryActionRef = useRef(false);

  const editorDirty = Boolean(
    editor && editorBaseline && JSON.stringify(editor) !== JSON.stringify(editorBaseline),
  );
  const currentStory = generated?.outcome === 'story' ? generated.story : null;

  useEffect(() => { onDirtyChange?.(editorDirty); }, [editorDirty, onDirtyChange]);
  useEffect(() => () => { onDirtyChange?.(false); }, [onDirtyChange]);

  const reportError = (value: unknown) => setError(errorMessage(value));
  const appendWarning = (message: string) => {
    setWarning((current) => current ? `${current} ${message}` : message);
  };
  const refreshBestEffort = async (completedAction: string) => {
    try {
      await refreshLibrary();
    } catch (refreshError) {
      appendWarning(
        `${completedAction}, but the library could not be refreshed: ${errorMessage(refreshError)}`,
      );
    }
  };

  useEffect(() => {
    refreshLibrary().catch(reportError);
  }, [refreshLibrary]);

  const stillOwns = (generation: number) => generationRef.current === generation;

  const invalidateGeneration = () => {
    generationRef.current += 1;
    generationOperationRef.current = null;
    assistingGenerationRef.current = null;
    setAssisting(false);
    // The disowned operation can no longer clear this itself because its
    // `finally` checks ownership. Release the page before replacement work.
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
      if (!stillOwns(generation)) return;
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
    } catch (assistError) {
      if (stillOwns(generation)) {
        setWarning(
          `Assisted narration was unavailable; showing the symbolic Story: ${errorMessage(assistError)}`,
        );
      }
    } finally {
      // Only the assist that owns the indicator may clear it. A stale assist
      // must not hide a newer request's progress.
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
      // Curating directly from the library starts without a Story hash. Keep
      // the displayed Story refreshable by naming its subject in the URL.
      onSubjectChange?.(displayedSubjectIri(symbolic));
      if (assistLevel === 1 && symbolic.outcome === 'story') {
        void improveNarration({ recipe_id: recipeId }, symbolic.story, generation);
      }
    } catch (reloadError) {
      if (stillOwns(generation)) {
        appendWarning(
          `${completedAction}, but its reader could not be reloaded: ${errorMessage(reloadError)}`,
        );
      }
    }
  };

  const generate = async (request: StorySelectionRequest, replacesCurrent = false) => {
    // Navigation-confirmed replacements supersede both visible and in-flight
    // work. Other callers defer while curation or generation owns the page.
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
      if (!stillOwns(generation)) return;
      setGenerated(symbolic);
      onSubjectChange?.(displayedSubjectIri(symbolic));
      setBusy(false);
      if (assistLevel === 1 && symbolic.outcome === 'story') {
        await improveNarration(request, symbolic.story, generation);
      }
    } catch (generationError) {
      if (stillOwns(generation)) setError(errorMessage(generationError));
    } finally {
      if (generationOperationRef.current === generation) generationOperationRef.current = null;
      if (stillOwns(generation)) setBusy(false);
    }
  };

  const resetForNavigation = () => {
    invalidateGeneration();
    setEditor(null);
    setEditorBaseline(null);
    setGenerated(null);
  };

  const replaceWith = (subjectIri: string) => {
    // URL synchronization echoes the displayed subject back to the hook. Do
    // not discard reader progress when the subject is already on screen.
    if (generated && displayedSubjectIri(generated) === subjectIri) return;
    setEditor(null);
    setEditorBaseline(null);
    setGenerated(null);
    void generate({ subject_iri: subjectIri }, true);
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
    } catch (editError) {
      if (stillOwns(generation)) setError(errorMessage(editError));
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
    } catch (saveError) {
      if (stillOwns(generation)) setError(errorMessage(saveError));
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
      } catch (publishError) {
        if (!stillOwns(generation)) return;
        setError(`Story changes were saved, but publication failed: ${errorMessage(publishError)}`);
        await refreshBestEffort('Story changes were saved');
      }
    } catch (saveError) {
      if (stillOwns(generation)) setError(errorMessage(saveError));
    } finally {
      editorOperationRef.current = false;
      if (stillOwns(generation)) setBusy(false);
    }
  };

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
    } catch (saveError) {
      if (stillOwns(generation)) setError(errorMessage(saveError));
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

  const closeReader = () => {
    invalidateGeneration();
    setGenerated(null);
    onSubjectChange?.(null);
  };

  return {
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
  };
}
