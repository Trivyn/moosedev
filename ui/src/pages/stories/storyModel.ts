import {
  StoryGenerateRequest,
  StoryRecipe,
  StoryRecipeSubject,
  StoryRun,
  StorySectionKind,
  StorySubjectCandidate,
} from '../../api/types';

export type StorySelectionRequest = Omit<StoryGenerateRequest, 'assist_level' | 'include_checks'>;

export const sectionLabels: Record<StorySectionKind, string> = {
  orientation: 'Orientation',
  evolution: 'Evolution',
  current_state: 'Current state',
  implementation: 'Implementation',
  implications: 'Implications',
};

export const sectionKinds = Object.keys(sectionLabels) as StorySectionKind[];

let fallbackIdSequence = 0;

function uuidEntropy(): string {
  try {
    const uuid = globalThis.crypto?.randomUUID?.();
    if (uuid) return uuid;
  } catch {
    // Fall through for restricted browser contexts.
  }
  fallbackIdSequence += 1;
  return `${Date.now().toString(36)}-${fallbackIdSequence.toString(36)}-${Math.random().toString(36).slice(2)}`;
}

function storyId(title: string): string {
  const stem = title.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 48);
  return `${stem || 'story'}-${uuidEntropy()}`;
}

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function unique<T>(values: T[]): T[] {
  return [...new Set(values)];
}

export function parseReferences(value: string): string[] {
  return value.split(/[\n,]+/).map((item) => item.trim()).filter(Boolean);
}

export function formatTimestamp(value?: string | null): string {
  if (!value) return 'Date not recorded';
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? value : date.toLocaleString();
}

export function formatBytes(bytes: number): string {
  if (bytes < 1_024) return `${bytes} B`;
  if (bytes < 1_048_576) return `${(bytes / 1_024).toFixed(1)} KiB`;
  return `${(bytes / 1_048_576).toFixed(1)} MiB`;
}

export function storySubjectIdentity(subject: StoryRun['subject']): string {
  return subject.type === 'entity' ? subject.iri : `topic:${subject.query}`;
}

export function storySelection(subject: StoryRun['subject']): StorySelectionRequest {
  return subject.type === 'entity' ? { subject_iri: subject.iri } : { topic: subject.query };
}

export function recipeFromRun(run: StoryRun): StoryRecipe {
  const subject: StoryRecipeSubject = run.subject.type === 'entity'
    ? { type: 'entity', iri: run.subject.iri }
    : { type: 'topic', query: run.subject.query };
  return {
    id: run.recipe_id || storyId(run.title),
    title: run.title,
    schema_version: 3,
    subject,
    goal: run.goal,
    audience: 'reboarding',
    focus: {
      include_record_iris: [],
      exclude_record_iris: [],
      include_code_symbols: [],
      exclude_code_symbols: [],
      emphasis: unique(run.narrative.map((section) => section.kind)),
    },
    status: 'draft',
    curator: 'maintainer',
  };
}

function storyStructureFingerprint(story: StoryRun): string {
  return JSON.stringify({
    schema_version: story.schema_version,
    recipe_id: story.recipe_id ?? null,
    trust_state: story.trust_state,
    title: story.title,
    subject: story.subject,
    goal: story.goal,
    narrative: story.narrative.map((section) => ({
      id: section.id,
      kind: section.kind,
      title: section.title,
    })),
    timeline: story.timeline,
    evidence: story.evidence,
    code_anchors: story.code_anchors,
    coverage: story.coverage,
    gaps: story.gaps,
  });
}

// LLM assistance may replace prose only; the deterministic projection and symbolic checks survive.
export function applyAssistedNarration(symbolic: StoryRun, assisted: StoryRun): StoryRun | null {
  if (storyStructureFingerprint(symbolic) !== storyStructureFingerprint(assisted)) return null;
  return {
    ...symbolic,
    narration_mode: assisted.narration_mode,
    narration_strategy: assisted.narration_strategy,
    narration_outcome: assisted.narration_outcome,
    narration_failure_reason: assisted.narration_failure_reason,
    narration_coverage: assisted.narration_coverage,
    brief: assisted.brief,
    narrative: assisted.narrative,
  };
}

export function filterStorySubjects(
  options: StorySubjectCandidate[],
  query: string,
): StorySubjectCandidate[] {
  const normalized = query.trim().toLocaleLowerCase();
  if (!normalized) return options;
  return options.filter((option) =>
    option.label.toLocaleLowerCase().includes(normalized) ||
    option.kind.toLocaleLowerCase().includes(normalized) ||
    option.description?.toLocaleLowerCase().includes(normalized),
  );
}

export function narrationNotice(story: StoryRun, assisting: boolean) {
  if (assisting) {
    return {
      severity: 'info' as const,
      text: 'The grounded Story is ready. MOOSEDev is improving its readability in the background; its evidence, chronology, gaps, and checks will not change.',
    };
  }
  if (story.narration_mode === 'llm' && story.narration_outcome === 'succeeded') {
    const coverage = story.narration_coverage;
    const scope = coverage?.truncated
      ? ` It used ${coverage.included_entities} of ${coverage.eligible_entities} eligible evidence entities; the complete dossier remains available below.`
      : '';
    return {
      severity: 'info' as const,
      text: 'The configured LLM shaped the bounded evidence packet into this narrative. '
        + 'MOOSEDev selected and validated the evidence, chronology, gaps, citations, and checks '
        + `deterministically.${scope}`,
    };
  }
  if (story.narration_outcome === 'not_requested') {
    return {
      severity: 'success' as const,
      text: 'This narrative was assembled deterministically from project knowledge; no LLM narration was used.',
    };
  }
  const reasons: Record<StoryRun['narration_outcome'], string> = {
    not_requested: '',
    succeeded: '',
    unconfigured: 'no narration provider is configured',
    ineligible: 'the evidence could not be safely packaged for narration',
    timeout: 'the narration request timed out',
    provider_error: 'the narration provider returned an error',
    invalid_response: 'the narration response failed grounding validation',
  };
  const invalidReasons: Record<NonNullable<StoryRun['narration_failure_reason']>, string> = {
    packet_too_large: 'the minimum grounded narration packet did not fit the configured model budget',
    invalid_json: 'the provider did not return readable JSON',
    schema_mismatch: 'the provider response did not match the narration schema',
    citation_mismatch: 'the provider response did not preserve the required evidence citations',
    structured_output_unsupported: 'the provider does not support the required structured-output contract',
  };
  const reason = story.narration_failure_reason
    ? invalidReasons[story.narration_failure_reason]
    : reasons[story.narration_outcome];
  return {
    severity: 'warning' as const,
    text: `LLM narration was not used because ${reason}. The complete symbolic Story is shown instead.`,
  };
}

export function validateRecipe(recipe: StoryRecipe): string[] {
  const errors: string[] = [];
  const groups: Array<[string, string[]]> = [
    ['included records', recipe.focus.include_record_iris],
    ['excluded records', recipe.focus.exclude_record_iris],
    ['included code symbols', recipe.focus.include_code_symbols],
    ['excluded code symbols', recipe.focus.exclude_code_symbols],
  ];
  for (const [label, values] of groups) {
    if (values.length > 128) errors.push(`${label} exceeds the 128-item limit`);
    if (unique(values).length !== values.length) errors.push(`${label} contains duplicates`);
  }
  if (recipe.focus.include_record_iris.some((iri) => recipe.focus.exclude_record_iris.includes(iri))) {
    errors.push('a record cannot be both included and excluded');
  }
  if (recipe.focus.include_code_symbols.some((symbol) => recipe.focus.exclude_code_symbols.includes(symbol))) {
    errors.push('a code symbol cannot be both included and excluded');
  }
  if (unique(recipe.focus.emphasis).length !== recipe.focus.emphasis.length) {
    errors.push('section emphasis contains duplicates');
  }
  if ((recipe.curator_context?.length ?? 0) > 2_000) errors.push('curator context exceeds 2,000 characters');
  if (!recipe.title.trim()) errors.push('title is required');
  if (!recipe.goal.trim()) errors.push('learning goal is required');
  return errors;
}
