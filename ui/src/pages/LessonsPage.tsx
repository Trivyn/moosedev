import { Alert, Typography } from '@mui/material';
import { api } from '../api/client';
import { LessonSummary, LessonWarnings } from '../api/types';
import GeneratedArtifactPage from '../components/artifacts/GeneratedArtifactPage';
import { ArtifactTarget } from '../components/artifacts/LinkedMarkdown';

interface LessonsPageProps {
  targetUuid?: string;
  onNavigateArtifact?: (target: ArtifactTarget) => void;
}

function warningCount(warnings: LessonWarnings) {
  return warnings.missing_description.length + warnings.unlinked_lessons.length;
}

function WarningSummary({ warnings }: { warnings: LessonWarnings }) {
  const parts = [
    warnings.missing_description.length ? `${warnings.missing_description.length} missing description` : null,
    warnings.unlinked_lessons.length ? `${warnings.unlinked_lessons.length} without source links` : null,
  ].filter(Boolean);

  return parts.length ? <Alert severity="warning">{parts.join(', ')}</Alert> : null;
}

function LessonListMeta(lesson: LessonSummary) {
  return (
    <Typography variant="caption" color="text.secondary">
      {lesson.related_sources} source{lesson.related_sources === 1 ? '' : 's'}
    </Typography>
  );
}

export default function LessonsPage({ targetUuid, onNavigateArtifact }: LessonsPageProps) {
  return (
    <GeneratedArtifactPage<
      LessonSummary,
      Awaited<ReturnType<typeof api.listLessons>>,
      LessonWarnings
    >
      targetUuid={targetUuid}
      onNavigateArtifact={onNavigateArtifact}
      artifactKind="lessons"
      title="Lessons"
      emptyText="No lessons recorded."
      selectText="Select a lesson."
      refreshTooltip="Refresh lessons"
      downloadTooltip="Download lessons archive"
      archiveFilename="moosedev-lessons.zip"
      sidebarMinWidth={380}
      sidebarMaxWidth={560}
      loadList={api.listLessons}
      loadDetail={api.getLesson}
      downloadArchive={api.downloadLessonArchive}
      recordsOf={(list) => list.lessons}
      generatedFileCount={(list) => list.lesson_files}
      warningsOf={(list) => list.warnings}
      warningCount={warningCount}
      renderWarningSummary={(warnings) => <WarningSummary warnings={warnings} />}
      recordPrefix="LSN"
      renderListMeta={LessonListMeta}
    />
  );
}
