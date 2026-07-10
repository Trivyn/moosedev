import { Alert, Typography } from '@mui/material';
import { api } from '../api/client';
import { ConstraintSummary, ConstraintWarnings } from '../api/types';
import GeneratedArtifactPage from '../components/artifacts/GeneratedArtifactPage';
import { ArtifactTarget } from '../components/artifacts/LinkedMarkdown';

interface ConstraintsPageProps {
  targetUuid?: string;
  onNavigateArtifact?: (target: ArtifactTarget) => void;
  onNavigateRecord?: (iri: string) => void;
}

function warningCount(warnings: ConstraintWarnings) {
  return warnings.missing_description.length + warnings.unlinked_constraints.length;
}

function WarningSummary({ warnings }: { warnings: ConstraintWarnings }) {
  const parts = [
    warnings.missing_description.length
      ? `${warnings.missing_description.length} missing description`
      : null,
    warnings.unlinked_constraints.length
      ? `${warnings.unlinked_constraints.length} without constrained targets`
      : null,
  ].filter(Boolean);

  return parts.length ? <Alert severity="warning">{parts.join(', ')}</Alert> : null;
}

function ConstraintListMeta(constraint: ConstraintSummary) {
  return (
    <Typography variant="caption" color="text.secondary">
      {constraint.related_targets} target{constraint.related_targets === 1 ? '' : 's'}
    </Typography>
  );
}

export default function ConstraintsPage({
  targetUuid,
  onNavigateArtifact,
  onNavigateRecord,
}: ConstraintsPageProps) {
  return (
    <GeneratedArtifactPage<
      ConstraintSummary,
      Awaited<ReturnType<typeof api.listConstraints>>,
      ConstraintWarnings
    >
      targetUuid={targetUuid}
      onNavigateArtifact={onNavigateArtifact}
      onNavigateRecord={onNavigateRecord}
      artifactKind="constraints"
      title="Constraints"
      emptyText="No constraints recorded."
      selectText="Select a constraint."
      refreshTooltip="Refresh constraints"
      downloadTooltip="Download constraints archive"
      archiveFilename="moosedev-constraints.zip"
      sidebarMinWidth={380}
      sidebarMaxWidth={560}
      loadList={api.listConstraints}
      loadDetail={api.getConstraint}
      downloadArchive={api.downloadConstraintArchive}
      recordsOf={(list) => list.constraints}
      generatedFileCount={(list) => list.constraint_files}
      warningsOf={(list) => list.warnings}
      warningCount={warningCount}
      renderWarningSummary={(warnings) => <WarningSummary warnings={warnings} />}
      recordPrefix="CST"
      renderListMeta={ConstraintListMeta}
    />
  );
}
