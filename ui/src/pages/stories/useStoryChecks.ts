import { useEffect, useRef, useState } from 'react';
import { api } from '../../api/client';
import { StoryCheckGradeResponse, StoryRun } from '../../api/types';
import { errorMessage, storySubjectIdentity } from './storyModel';

function withoutKey<T>(values: Record<string, T>, key: string): Record<string, T> {
  const next = { ...values };
  delete next[key];
  return next;
}

export function useStoryChecks(story: StoryRun) {
  const [selected, setSelected] = useState<Record<string, string>>({});
  const [results, setResults] = useState<Record<string, StoryCheckGradeResponse>>({});
  const [gradeErrors, setGradeErrors] = useState<Record<string, string>>({});
  const [grading, setGrading] = useState<Record<string, boolean>>({});
  const requestByCheck = useRef<Record<string, number>>({});

  // Prose is deliberately excluded so a background narration upgrade preserves quiz progress.
  const checkIdentity = JSON.stringify([
    story.recipe_id ?? storySubjectIdentity(story.subject),
    story.checks.map((check) => check.id),
  ]);

  useEffect(() => {
    requestByCheck.current = {};
    setSelected({});
    setResults({});
    setGradeErrors({});
    setGrading({});
    return () => { requestByCheck.current = {}; };
  }, [checkIdentity]);

  const selectAnswer = (checkId: string, optionId: string) => {
    requestByCheck.current[checkId] = (requestByCheck.current[checkId] ?? 0) + 1;
    setSelected((current) => ({ ...current, [checkId]: optionId }));
    setGrading((current) => ({ ...current, [checkId]: false }));
    setResults((current) => withoutKey(current, checkId));
    setGradeErrors((current) => withoutKey(current, checkId));
  };

  const grade = async (checkId: string) => {
    const optionId = selected[checkId];
    if (!optionId) return;
    const request = (requestByCheck.current[checkId] ?? 0) + 1;
    requestByCheck.current[checkId] = request;
    setGrading((current) => ({ ...current, [checkId]: true }));
    setGradeErrors((current) => withoutKey(current, checkId));
    try {
      const result = await api.gradeStoryCheck({
        check_id: checkId,
        selected_option_ids: [optionId],
      });
      if (requestByCheck.current[checkId] !== request) return;
      setResults((current) => ({ ...current, [checkId]: result }));
      if (!result.correct && result.revisit_section_id) {
        document
          .getElementById(`story-section-${result.revisit_section_id}`)
          ?.scrollIntoView({ behavior: 'smooth' });
      }
    } catch (error) {
      if (requestByCheck.current[checkId] === request) {
        setGradeErrors((current) => ({ ...current, [checkId]: errorMessage(error) }));
      }
    } finally {
      if (requestByCheck.current[checkId] === request) {
        setGrading((current) => ({ ...current, [checkId]: false }));
      }
    }
  };

  return { selected, results, gradeErrors, grading, selectAnswer, grade };
}
