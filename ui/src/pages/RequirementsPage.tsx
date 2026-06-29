import { Alert, Chip, Stack, Typography } from '@mui/material';
import { api } from '../api/client';
import { RequirementSummary, RequirementWarnings } from '../api/types';
import GeneratedArtifactPage from '../components/artifacts/GeneratedArtifactPage';
import { ArtifactTarget } from '../components/artifacts/LinkedMarkdown';

interface RequirementsPageProps {
  targetIri?: string;
  onNavigateArtifact?: (target: ArtifactTarget) => void;
}

function warningCount(warnings: RequirementWarnings) {
  return warnings.missing_description.length + warnings.unlinked_requirements.length;
}

function WarningSummary({ warnings }: { warnings: RequirementWarnings }) {
  const parts = [
    warnings.missing_description.length ? `${warnings.missing_description.length} missing description` : null,
    warnings.unlinked_requirements.length ? `${warnings.unlinked_requirements.length} without ADR links` : null,
  ].filter(Boolean);

  return parts.length ? <Alert severity="warning">{parts.join(', ')}</Alert> : null;
}

function RequirementListMeta(requirement: RequirementSummary) {
  return (
    <Stack direction="row" spacing={1} alignItems="center" useFlexGap flexWrap="wrap">
      <Chip
        size="small"
        label={requirement.addressed ? 'Addressed' : 'Open'}
        color={requirement.addressed ? 'success' : 'default'}
        variant={requirement.addressed ? 'filled' : 'outlined'}
      />
      <Typography variant="caption" color="text.secondary">
        {requirement.related_adrs} ADR{requirement.related_adrs === 1 ? '' : 's'}
      </Typography>
    </Stack>
  );
}

export default function RequirementsPage({ targetIri, onNavigateArtifact }: RequirementsPageProps) {
  return (
    <GeneratedArtifactPage<
      RequirementSummary,
      Awaited<ReturnType<typeof api.listRequirements>>,
      RequirementWarnings
    >
      targetIri={targetIri}
      onNavigateArtifact={onNavigateArtifact}
      title="Requirements"
      emptyText="No requirements recorded."
      selectText="Select a requirement."
      refreshTooltip="Refresh requirements"
      downloadTooltip="Download requirements archive"
      archiveFilename="moosedev-requirements.zip"
      sidebarMinWidth={380}
      sidebarMaxWidth={560}
      loadList={api.listRequirements}
      loadDetail={api.getRequirement}
      downloadArchive={api.downloadRequirementArchive}
      recordsOf={(list) => list.requirements}
      generatedFileCount={(list) => list.requirement_files}
      warningsOf={(list) => list.warnings}
      warningCount={warningCount}
      renderWarningSummary={(warnings) => <WarningSummary warnings={warnings} />}
      recordPrefix="REQ"
      renderListMeta={RequirementListMeta}
    />
  );
}
