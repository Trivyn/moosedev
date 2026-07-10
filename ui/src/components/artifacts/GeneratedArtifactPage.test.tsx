// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import GeneratedArtifactPage, { ArtifactSummaryBase } from './GeneratedArtifactPage';

afterEach(cleanup);

interface TestList {
  records: ArtifactSummaryBase[];
}

const records: ArtifactSummaryBase[] = [
  {
    num: '0001',
    title: 'First decision',
    status: 'Accepted',
    date: '2026-07-08',
    author: 'tester',
    iri: 'https://moosedev.dev/kg/ArchitecturalDecision/adr-1',
  },
  {
    num: '0002',
    title: 'Linked decision',
    status: 'Accepted',
    date: '2026-07-09',
    author: 'tester',
    iri: 'https://moosedev.dev/kg/ArchitecturalDecision/adr-2',
  },
];

describe('GeneratedArtifactPage direct links', () => {
  it('selects the UUID target before loading detail and makes list clicks addressable', async () => {
    const loadDetail = vi.fn(async (num: string) => ({
      summary: records.find((record) => record.num === num)!,
      markdown: `Detail ${num}`,
    }));
    const onNavigateArtifact = vi.fn();

    render(
      <GeneratedArtifactPage<ArtifactSummaryBase, TestList, null>
        targetUuid="adr-2"
        onNavigateArtifact={onNavigateArtifact}
        artifactKind="adrs"
        title="ADRs"
        emptyText="Empty"
        selectText="Select"
        refreshTooltip="Refresh"
        downloadTooltip="Download"
        archiveFilename="records.zip"
        sidebarMinWidth={300}
        sidebarMaxWidth={500}
        loadList={async () => ({ records })}
        loadDetail={loadDetail}
        downloadArchive={async () => new Blob()}
        recordsOf={(list) => list.records}
        generatedFileCount={(list) => list.records.length}
        warningsOf={() => null}
        warningCount={() => 0}
        renderWarningSummary={() => null}
      />,
    );

    expect(await screen.findByText('Detail 0002')).toBeInTheDocument();
    expect(loadDetail).toHaveBeenCalledTimes(1);
    expect(loadDetail).toHaveBeenCalledWith('0002');

    fireEvent.click(screen.getByText('First decision'));

    await waitFor(() => expect(screen.getByText('Detail 0001')).toBeInTheDocument());
    expect(onNavigateArtifact).toHaveBeenCalledWith({
      kind: 'adrs',
      iri: 'https://moosedev.dev/kg/ArchitecturalDecision/adr-1',
    });
  });
});
