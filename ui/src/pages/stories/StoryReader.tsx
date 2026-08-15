import { useMemo } from 'react';
import {
  Accordion,
  AccordionDetails,
  AccordionSummary,
  Alert,
  Box,
  Button,
  Chip,
  Divider,
  FormControlLabel,
  Paper,
  Radio,
  RadioGroup,
  Stack,
  Typography,
} from '@mui/material';
import EditOutlinedIcon from '@mui/icons-material/EditOutlined';
import ExpandMoreIcon from '@mui/icons-material/ExpandMore';
import SaveOutlinedIcon from '@mui/icons-material/SaveOutlined';
import { isWorkingSetStatus } from '../../utils/lifecycle';
import {
  StoryEvidenceDetail,
  StoryParagraph,
  StoryRun,
  StoryTimelineEvent,
  StoryTrustState,
} from '../../api/types';
import {
  formatBytes,
  formatTimestamp,
  narrationNotice,
  sectionLabels,
} from './storyModel';
import { useStoryChecks } from './useStoryChecks';

const trustColors: Record<StoryTrustState, 'default' | 'info' | 'success'> = {
  generated: 'info',
  draft: 'default',
  published: 'success',
};

export function TrustBadge({ state }: { state: StoryTrustState }) {
  const label = `${state[0].toUpperCase()}${state.slice(1)} Story`;
  return <Chip size="small" color={trustColors[state]} label={label} />;
}

interface CitationLinksProps {
  paragraph: StoryParagraph;
  evidenceByIri: Map<string, StoryEvidenceDetail>;
  citationNumber: Map<string, number>;
  onNavigateRecord: (iri: string) => void;
}

function CitationLinks({
  paragraph,
  evidenceByIri,
  citationNumber,
  onNavigateRecord,
}: CitationLinksProps) {
  const visibleCitations = paragraph.citation_iris.filter(
    (iri) => !evidenceByIri.get(iri)?.suppressed,
  );
  if (!visibleCitations.length) return null;
  return (
    <Box
      component="span"
      sx={{ display: 'inline-flex', gap: 0.25, ml: 0.5, verticalAlign: 'baseline' }}
    >
      {visibleCitations.map((iri) => {
        const evidence = evidenceByIri.get(iri);
        const number = citationNumber.get(iri);
        return (
          <Button
            key={iri}
            size="small"
            variant="text"
            aria-label={`Evidence ${number}: ${evidence?.title ?? iri}`}
            title={`${evidence?.kind ?? 'Evidence'}: ${evidence?.title ?? iri}`}
            onClick={() => onNavigateRecord(iri)}
            sx={{ minWidth: 0, p: 0.25, lineHeight: 1, verticalAlign: 'super' }}
          >
            [{number ?? '?'}]
          </Button>
        );
      })}
    </Box>
  );
}

interface StoryTimelineProps {
  timeline: StoryTimelineEvent[];
  evidenceByIri: Map<string, StoryEvidenceDetail>;
  onNavigateRecord: (iri: string) => void;
}

function StoryTimeline({ timeline, evidenceByIri, onNavigateRecord }: StoryTimelineProps) {
  if (!timeline.length) return null;
  return (
    <Box component="section" aria-labelledby="story-timeline-heading" sx={{ mt: 6 }}>
      <Typography id="story-timeline-heading" variant="h4">Evolution over time</Typography>
      <Typography variant="body2" color="text.secondary" sx={{ mt: 0.5, mb: 2 }}>
        Lifecycle events and supersessions are ordered from the project graph.
      </Typography>
      <Stack component="ol" spacing={2} sx={{ pl: 2.5 }}>
        {timeline.map((event) => {
          const relations: Array<{ label: string; iris: string[] }> = [
            { label: 'Supersedes', iris: event.predecessor_iris },
            { label: 'Superseded by', iris: event.successor_iris },
            { label: 'Rationale', iris: event.rationale_iris },
          ];
          return (
            <Box component="li" key={event.id} sx={{ pl: 1 }}>
              <Button
                variant="text"
                onClick={() => onNavigateRecord(event.evidence_iri)}
                sx={{ p: 0, textAlign: 'left', justifyContent: 'flex-start' }}
              >
                {event.title}
              </Button>
              <Stack direction="row" spacing={0.75} useFlexGap flexWrap="wrap" sx={{ my: 0.5 }}>
                <Chip size="small" label={event.kind} />
                <Chip
                  size="small"
                  variant="outlined"
                  color={isWorkingSetStatus(event.status) ? 'success' : 'warning'}
                  label={event.status || 'unknown'}
                />
                <Chip size="small" variant="outlined" label={formatTimestamp(event.timestamp)} />
              </Stack>
              {event.relation ? <Typography variant="body2">{event.relation}</Typography> : null}
              {relations.map(({ label, iris }) => iris.length ? (
                <Stack
                  key={label}
                  direction="row"
                  spacing={0.5}
                  alignItems="baseline"
                  useFlexGap
                  flexWrap="wrap"
                >
                  <Typography variant="caption" color="text.secondary">{label}:</Typography>
                  {iris.map((iri) => (
                    <Button
                      key={iri}
                      size="small"
                      onClick={() => onNavigateRecord(iri)}
                      sx={{ minWidth: 0, p: 0 }}
                    >
                      {evidenceByIri.get(iri)?.title ?? iri}
                    </Button>
                  ))}
                </Stack>
              ) : null)}
            </Box>
          );
        })}
      </Stack>
    </Box>
  );
}

function CodeAnchors({ story, onNavigateRecord }: {
  story: StoryRun;
  onNavigateRecord: (iri: string) => void;
}) {
  if (!story.code_anchors.length) return null;
  return (
    <Box component="section" sx={{ mt: 5 }}>
      <Typography variant="h5" gutterBottom>Code anchors</Typography>
      <Stack direction="row" spacing={1} useFlexGap flexWrap="wrap">
        {story.code_anchors.map((anchor) => (
          <Chip
            key={anchor.symbol}
            clickable={Boolean(anchor.entity_iri)}
            label={`${anchor.label}${anchor.path ? ` · ${anchor.path}${anchor.line != null ? `:${anchor.line}` : ''}` : ''}`}
            onClick={() => anchor.entity_iri && onNavigateRecord(anchor.entity_iri)}
          />
        ))}
      </Stack>
    </Box>
  );
}

function EvidenceEntry({ item, number, onNavigateRecord }: {
  item: StoryEvidenceDetail;
  number: number;
  onNavigateRecord: (iri: string) => void;
}) {
  const summaryValues = [item.title, item.status, item.description, item.timestamp, item.author];
  const visibleProperties = item.properties.filter(
    (property) => !summaryValues.includes(property.value),
  );
  const attribution = [
    item.author ? `By ${item.author}` : null,
    item.timestamp ? formatTimestamp(item.timestamp) : null,
  ].filter(Boolean).join(' · ') || 'No authorship or timestamp recorded';

  return (
    <Box>
      <Stack direction="row" spacing={1} alignItems="center" useFlexGap flexWrap="wrap">
        <Typography variant="subtitle2">[{number}]</Typography>
        <Button variant="text" onClick={() => onNavigateRecord(item.iri)} sx={{ p: 0 }}>
          {item.title}
        </Button>
        <Chip size="small" label={item.kind} />
        <Chip
          size="small"
          variant="outlined"
          color={isWorkingSetStatus(item.status) ? 'success' : 'warning'}
          label={item.status || 'unknown'}
        />
      </Stack>
      {item.description ? (
        <Typography variant="body2" sx={{ mt: 0.75, whiteSpace: 'pre-wrap' }}>
          {item.description}
        </Typography>
      ) : null}
      <Typography variant="caption" color="text.secondary">{attribution}</Typography>
      {visibleProperties.length ? (
        <Box
          component="dl"
          sx={{
            display: 'grid',
            gridTemplateColumns: 'max-content minmax(0, 1fr)',
            columnGap: 1.5,
            rowGap: 0.5,
            mt: 1,
            mb: 0,
          }}
        >
          {visibleProperties.map((property, propertyIndex) => (
            <Box
              key={`${property.predicate}-${property.value}-${propertyIndex}`}
              sx={{ display: 'contents' }}
            >
              <Typography component="dt" variant="caption" color="text.secondary">
                {property.label}
              </Typography>
              <Typography component="dd" variant="caption" sx={{ m: 0, whiteSpace: 'pre-wrap' }}>
                {property.value}
              </Typography>
            </Box>
          ))}
        </Box>
      ) : null}
      {item.relations.length ? (
        <Stack spacing={0.5} sx={{ mt: 1 }}>
          {item.relations.map((relation, relationIndex) => (
            <Typography
              key={`${relation.predicate}-${relation.target_iri}-${relationIndex}`}
              variant="caption"
              color="text.secondary"
            >
              {relation.direction === 'incoming' ? '←' : '→'} {relation.label}{' '}
              <Button
                size="small"
                onClick={() => onNavigateRecord(relation.target_iri)}
                sx={{ minWidth: 0, p: 0, verticalAlign: 'baseline' }}
              >
                {relation.target_label}
              </Button>{' '}
              ({relation.target_kind})
            </Typography>
          ))}
        </Stack>
      ) : null}
    </Box>
  );
}

interface EvidenceAppendixProps {
  story: StoryRun;
  citationNumber: Map<string, number>;
  onNavigateRecord: (iri: string) => void;
}

function EvidenceAppendix({ story, citationNumber, onNavigateRecord }: EvidenceAppendixProps) {
  const evidenceGroups = useMemo(() => {
    const groups = new Map<string, Array<{ item: StoryEvidenceDetail; number: number }>>();
    story.evidence.filter((item) => !item.suppressed).forEach((item, index) => {
      const group = groups.get(item.kind) ?? [];
      group.push({ item, number: index + 1 });
      groups.set(item.kind, group);
    });
    return [...groups.entries()].sort(([left], [right]) => left.localeCompare(right));
  }, [story.evidence]);

  return (
    <Box component="section" sx={{ mt: 5 }}>
      <Accordion variant="outlined">
        <AccordionSummary
          expandIcon={<ExpandMoreIcon />}
          aria-controls="story-evidence-content"
          id="story-evidence-heading"
        >
          <Box>
            <Typography variant="h6">Evidence appendix</Typography>
            <Typography variant="caption" color="text.secondary">
              {citationNumber.size} visible sources · {story.coverage.current_count} current ·{' '}
              {story.coverage.historical_count} historical · {story.coverage.proposed_count} proposed
            </Typography>
          </Box>
        </AccordionSummary>
        <AccordionDetails id="story-evidence-content">
          <Stack spacing={3}>
            <Box aria-label="Story projection coverage">
              <Typography variant="overline" color="text.secondary">Projection coverage</Typography>
              <Stack direction="row" spacing={0.75} useFlexGap flexWrap="wrap">
                <Chip
                  size="small"
                  variant="outlined"
                  label={`${formatBytes(story.coverage.dossier_bytes)} dossier`}
                />
                {story.coverage.subject_families.map((family) => (
                  <Chip key={family} size="small" variant="outlined" label={family} />
                ))}
                {story.coverage.outline_sections.map((kind) => (
                  <Chip
                    key={kind}
                    size="small"
                    color="primary"
                    variant="outlined"
                    label={sectionLabels[kind]}
                  />
                ))}
              </Stack>
            </Box>
            {evidenceGroups.map(([kind, entries]) => (
              <Box key={kind}>
                <Typography variant="overline" color="text.secondary">{kind}</Typography>
                <Stack spacing={2} divider={<Divider flexItem />}>
                  {entries.map(({ item, number }) => (
                    <EvidenceEntry
                      key={item.iri}
                      item={item}
                      number={number}
                      onNavigateRecord={onNavigateRecord}
                    />
                  ))}
                </Stack>
              </Box>
            ))}
          </Stack>
        </AccordionDetails>
      </Accordion>
    </Box>
  );
}

function KnowledgeGaps({ story }: { story: StoryRun }) {
  if (!story.gaps.length) return null;
  return (
    <Paper
      variant="outlined"
      sx={{
        p: { xs: 2, md: 3 },
        borderColor: 'warning.main',
        maxWidth: 980,
        width: '100%',
        mx: 'auto',
      }}
    >
      <Typography variant="h5">Knowledge gaps</Typography>
      <Typography variant="body2" color="text.secondary" sx={{ mb: 1.5 }}>
        These are missing or pending parts of the project record, not narrative guesses.
      </Typography>
      <Stack spacing={1}>
        {story.gaps.map((gap) => (
          <Alert key={gap.id} severity="warning" variant="outlined">
            <Typography variant="subtitle2">{gap.title}</Typography>
            <Typography variant="body2">{gap.detail}</Typography>
          </Alert>
        ))}
      </Stack>
    </Paper>
  );
}

function StoryChecks({ story }: { story: StoryRun }) {
  const { selected, results, gradeErrors, grading, selectAnswer, grade } = useStoryChecks(story);
  if (!story.checks.length) return null;
  return (
    <Paper
      variant="outlined"
      sx={{ p: { xs: 2, md: 3 }, maxWidth: 980, width: '100%', mx: 'auto' }}
    >
      <Typography variant="h5">Check your understanding</Typography>
      <Typography variant="body2" color="text.secondary" sx={{ mb: 2 }}>
        Answers are checked against graph relationships, not generated prose.
      </Typography>
      <Stack spacing={2.5}>
        {story.checks.map((check) => (
          <Box key={check.id}>
            <Typography variant="subtitle2">{check.question}</Typography>
            <RadioGroup
              value={selected[check.id] ?? ''}
              onChange={(event) => selectAnswer(check.id, event.target.value)}
            >
              {check.options.map((option) => (
                <FormControlLabel
                  key={option.id}
                  value={option.id}
                  control={<Radio size="small" />}
                  label={option.label}
                />
              ))}
            </RadioGroup>
            <Button
              size="small"
              disabled={!selected[check.id] || grading[check.id]}
              onClick={() => grade(check.id)}
            >
              Check answer
            </Button>
            {results[check.id] ? (
              <Alert severity={results[check.id].correct ? 'success' : 'info'} sx={{ mt: 1 }}>
                {results[check.id].feedback}
              </Alert>
            ) : null}
            {gradeErrors[check.id] ? (
              <Alert severity="error" sx={{ mt: 1 }}>{gradeErrors[check.id]}</Alert>
            ) : null}
          </Box>
        ))}
      </Stack>
    </Paper>
  );
}

interface StoryReaderProps {
  story: StoryRun;
  onNavigateRecord: (iri: string) => void;
  onSaveDraft: () => void;
  onCurate: () => void;
  onClose: () => void;
  onGenerateFresh: () => void;
  busy: boolean;
  assisting: boolean;
}

export default function StoryReader({
  story,
  onNavigateRecord,
  onSaveDraft,
  onCurate,
  onClose,
  onGenerateFresh,
  busy,
  assisting,
}: StoryReaderProps) {
  const evidenceByIri = useMemo(
    () => new Map(story.evidence.map((item) => [item.iri, item])),
    [story.evidence],
  );
  const citationNumber = useMemo(() => new Map(
    story.evidence
      .filter((item) => !item.suppressed)
      .map((item, index) => [item.iri, index + 1]),
  ), [story.evidence]);
  const provenance = narrationNotice(story, assisting);

  return (
    <Stack spacing={3}>
      <Paper
        component="article"
        variant="outlined"
        sx={{ p: { xs: 2.5, md: 5 }, maxWidth: 980, width: '100%', mx: 'auto' }}
      >
        <Stack
          direction={{ xs: 'column', md: 'row' }}
          spacing={2}
          justifyContent="space-between"
          alignItems={{ md: 'flex-start' }}
        >
          <Box>
            <Stack direction="row" spacing={1} alignItems="center" useFlexGap flexWrap="wrap">
              <TrustBadge state={story.trust_state} />
              <Chip
                size="small"
                variant="outlined"
                label={story.narration_mode === 'llm'
                  ? 'LLM-assisted narrative'
                  : 'Symbolic narrative'}
              />
              {assisting ? (
                <Chip size="small" color="info" variant="outlined" label="Improving readability…" />
              ) : null}
            </Stack>
            <Typography variant="h3" sx={{ mt: 1.5 }}>{story.title}</Typography>
            <Typography variant="subtitle1" color="text.secondary">{story.subject.label}</Typography>
          </Box>
          <Stack direction="row" spacing={1} useFlexGap flexWrap="wrap">
            <Button disabled={busy} onClick={onClose}>All Stories</Button>
            {story.trust_state === 'generated' ? (
              <Button
                disabled={busy}
                variant="outlined"
                startIcon={<SaveOutlinedIcon />}
                onClick={onSaveDraft}
              >
                Save as draft
              </Button>
            ) : (
              <>
                <Button disabled={busy} variant="outlined" onClick={onGenerateFresh}>
                  Generate fresh
                </Button>
                <Button
                  disabled={busy}
                  variant="outlined"
                  startIcon={<EditOutlinedIcon />}
                  onClick={onCurate}
                >
                  Curate
                </Button>
              </>
            )}
          </Stack>
        </Stack>

        <Typography variant="h6" component="p" sx={{ mt: 4, maxWidth: 820, lineHeight: 1.65 }}>
          {story.brief.text}
          <CitationLinks
            paragraph={story.brief}
            evidenceByIri={evidenceByIri}
            citationNumber={citationNumber}
            onNavigateRecord={onNavigateRecord}
          />
        </Typography>
        <Typography variant="body2" color="text.secondary" sx={{ mt: 1.5 }}>
          <strong>Reading goal:</strong> {story.goal}
        </Typography>
        {story.curator_context ? (
          <Alert severity="info" variant="outlined" sx={{ mt: 2 }}>
            <strong>Maintainer context (non-authoritative):</strong> {story.curator_context}
          </Alert>
        ) : null}
        <Alert severity={provenance.severity} variant="outlined" sx={{ mt: 2.5 }}>
          {provenance.text}
        </Alert>
        {story.coverage.truncated ? (
          <Alert severity="warning" sx={{ mt: 2 }}>
            The evidence closure reached its safety limit. The gaps section identifies this Story as incomplete.
          </Alert>
        ) : null}

        <Box sx={{ mt: 5 }}>
          {story.narrative.map((section, index) => (
            <Box
              id={`story-section-${section.id}`}
              key={section.id}
              component="section"
              sx={{ mt: index ? 5 : 0, scrollMarginTop: 24 }}
            >
              <Typography variant="overline" color="primary">
                {sectionLabels[section.kind]}
              </Typography>
              <Typography variant="h4" sx={{ mb: 2 }}>{section.title}</Typography>
              <Stack spacing={2}>
                {section.paragraphs.map((paragraph, paragraphIndex) => (
                  <Typography
                    key={`${section.id}-${paragraphIndex}`}
                    variant="body1"
                    sx={{ whiteSpace: 'pre-wrap', lineHeight: 1.8, maxWidth: 850 }}
                  >
                    {paragraph.text}
                    <CitationLinks
                      paragraph={paragraph}
                      evidenceByIri={evidenceByIri}
                      citationNumber={citationNumber}
                      onNavigateRecord={onNavigateRecord}
                    />
                  </Typography>
                ))}
              </Stack>
            </Box>
          ))}
        </Box>

        <StoryTimeline
          timeline={story.timeline}
          evidenceByIri={evidenceByIri}
          onNavigateRecord={onNavigateRecord}
        />
        <CodeAnchors story={story} onNavigateRecord={onNavigateRecord} />
        <EvidenceAppendix
          story={story}
          citationNumber={citationNumber}
          onNavigateRecord={onNavigateRecord}
        />
      </Paper>

      <KnowledgeGaps story={story} />
      <StoryChecks story={story} />
    </Stack>
  );
}
