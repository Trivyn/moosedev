// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import GeneratedArtifactPage, { ArtifactSummaryBase } from './GeneratedArtifactPage';

vi.mock('../graph/RecordNeighborhoodGraph', () => ({
  default: ({
    uuid,
    onNavigateRecord,
  }: {
    uuid: string;
    onNavigateRecord?: (iri: string) => void;
  }) => (
    <button onClick={() => onNavigateRecord?.('https://moosedev.dev/kg/Constraint/constraint-1')}>
      Relationship graph for {uuid}
    </button>
  ),
}));

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
    search_text: '# First decision\n\nUses a metadata-only-author.',
  },
  {
    num: '0002',
    title: 'Linked decision',
    status: 'Accepted',
    date: '2026-07-09',
    author: 'tester',
    iri: 'https://moosedev.dev/kg/ArchitecturalDecision/adr-2',
    search_text: '# Linked decision\n\nStores its durable state in RocksDB.',
  },
];

describe('GeneratedArtifactPage direct links', () => {
  it('selects the UUID target before loading detail and makes list clicks addressable', async () => {
    const loadDetail = vi.fn(async (num: string) => ({
      summary: records.find((record) => record.num === num)!,
      markdown: `Detail ${num}`,
    }));
    const onNavigateArtifact = vi.fn();
    const onNavigateRecord = vi.fn();

    render(
      <GeneratedArtifactPage<ArtifactSummaryBase, TestList, null>
        targetUuid="adr-2"
        onNavigateArtifact={onNavigateArtifact}
        onNavigateRecord={onNavigateRecord}
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
    expect(screen.getByText('Relationship graph for adr-2')).toBeInTheDocument();

    fireEvent.click(screen.getByText('Relationship graph for adr-2'));
    expect(onNavigateRecord).toHaveBeenCalledWith(
      'https://moosedev.dev/kg/Constraint/constraint-1',
    );

    fireEvent.click(screen.getByText('First decision'));

    await waitFor(() => expect(screen.getByText('Detail 0001')).toBeInTheDocument());
    expect(onNavigateArtifact).toHaveBeenCalledWith({
      kind: 'adrs',
      iri: 'https://moosedev.dev/kg/ArchitecturalDecision/adr-1',
    });
  });

  it('filters complete artifact content without changing the current selection', async () => {
    const loadDetail = vi.fn(async (num: string) => ({
      summary: records.find((record) => record.num === num)!,
      markdown: `Detail ${num}`,
    }));

    render(
      <GeneratedArtifactPage<ArtifactSummaryBase, TestList, null>
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

    expect(await screen.findByText('Detail 0001')).toBeInTheDocument();
    const search = screen.getByRole('searchbox', { name: 'Search records' });

    fireEvent.change(search, { target: { value: '  ROCKSDB  ' } });
    expect(within(screen.getByRole('list')).queryByText('First decision')).not.toBeInTheDocument();
    expect(within(screen.getByRole('list')).getByText('Linked decision')).toBeInTheDocument();
    expect(screen.getByText('Detail 0001')).toBeInTheDocument();
    expect(loadDetail).toHaveBeenCalledTimes(1);

    fireEvent.change(search, { target: { value: 'metadata-only-author' } });
    expect(within(screen.getByRole('list')).getByText('First decision')).toBeInTheDocument();
    expect(within(screen.getByRole('list')).queryByText('Linked decision')).not.toBeInTheDocument();

    fireEvent.change(search, { target: { value: 'no such artifact' } });
    expect(screen.getByText('No records match “no such artifact”.')).toBeInTheDocument();
    expect(screen.queryByRole('list')).not.toBeInTheDocument();

    fireEvent.change(search, { target: { value: '' } });
    expect(within(screen.getByRole('list')).getByText('First decision')).toBeInTheDocument();
    expect(within(screen.getByRole('list')).getByText('Linked decision')).toBeInTheDocument();
  });
});
