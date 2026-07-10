import { Alert } from '@mui/material';
import { api } from '../api/client';
import { AdrSummary, AdrWarnings } from '../api/types';
import GeneratedArtifactPage from '../components/artifacts/GeneratedArtifactPage';
import { ArtifactTarget } from '../components/artifacts/LinkedMarkdown';

interface AdrsPageProps {
  targetUuid?: string;
  onNavigateArtifact?: (target: ArtifactTarget) => void;
  onNavigateRecord?: (iri: string) => void;
}

function warningCount(warnings: AdrWarnings) {
  return (
    warnings.missing_context.length +
    warnings.missing_decision.length +
    warnings.missing_successor.length +
    warnings.missing_reciprocal.length
  );
}

function WarningSummary({ warnings }: { warnings: AdrWarnings }) {
  const parts = [
    warnings.missing_context.length ? `${warnings.missing_context.length} missing context` : null,
    warnings.missing_decision.length ? `${warnings.missing_decision.length} missing decision` : null,
    warnings.missing_successor.length ? `${warnings.missing_successor.length} missing successor` : null,
    warnings.missing_reciprocal.length ? `${warnings.missing_reciprocal.length} missing reciprocal` : null,
  ].filter(Boolean);

  return parts.length ? <Alert severity="warning">{parts.join(', ')}</Alert> : null;
}

export default function AdrsPage({ targetUuid, onNavigateArtifact, onNavigateRecord }: AdrsPageProps) {
  return (
    <GeneratedArtifactPage<AdrSummary, Awaited<ReturnType<typeof api.listAdrs>>, AdrWarnings>
      targetUuid={targetUuid}
      onNavigateArtifact={onNavigateArtifact}
      onNavigateRecord={onNavigateRecord}
      artifactKind="adrs"
      title="ADRs"
      emptyText="No architectural decisions recorded."
      selectText="Select an ADR."
      refreshTooltip="Refresh ADRs"
      downloadTooltip="Download ADR archive"
      archiveFilename="moosedev-adrs.zip"
      sidebarMinWidth={360}
      sidebarMaxWidth={540}
      loadList={api.listAdrs}
      loadDetail={api.getAdr}
      downloadArchive={api.downloadAdrArchive}
      recordsOf={(list) => list.adrs}
      generatedFileCount={(list) => list.adr_files}
      warningsOf={(list) => list.warnings}
      warningCount={warningCount}
      renderWarningSummary={(warnings) => <WarningSummary warnings={warnings} />}
    />
  );
}
