import { ReactNode, useEffect, useMemo, useState } from 'react';
import {
  Alert,
  Box,
  Button,
  Chip,
  CircularProgress,
  Divider,
  List,
  ListItemButton,
  Stack,
  TextField,
  Tooltip,
  Typography,
} from '@mui/material';
import DownloadIcon from '@mui/icons-material/Download';
import RefreshIcon from '@mui/icons-material/Refresh';
import LinkedMarkdown, { ArtifactTarget } from './LinkedMarkdown';
import RecordNeighborhoodGraph from '../graph/RecordNeighborhoodGraph';

export type ArtifactStatusColor = 'default' | 'primary' | 'success' | 'warning';

export interface ArtifactSummaryBase {
  num: string;
  title: string;
  status: string;
  date: string;
  author: string;
  iri: string;
  search_text: string;
}

interface ArtifactDetail<TSummary extends ArtifactSummaryBase> {
  summary: TSummary;
  markdown: string;
}

interface GeneratedArtifactPageProps<TSummary extends ArtifactSummaryBase, TList, TWarnings> {
  targetUuid?: string;
  onNavigateArtifact?: (target: ArtifactTarget) => void;
  onNavigateRecord?: (iri: string) => void;
  artifactKind: ArtifactTarget['kind'];
  title: string;
  emptyText: string;
  selectText: string;
  refreshTooltip: string;
  downloadTooltip: string;
  archiveFilename: string;
  sidebarMinWidth: number;
  sidebarMaxWidth: number;
  loadList: () => Promise<TList>;
  loadDetail: (num: string) => Promise<ArtifactDetail<TSummary>>;
  downloadArchive: () => Promise<Blob>;
  recordsOf: (list: TList) => TSummary[];
  generatedFileCount: (list: TList) => number;
  warningsOf: (list: TList) => TWarnings;
  warningCount: (warnings: TWarnings) => number;
  renderWarningSummary: (warnings: TWarnings) => ReactNode;
  recordPrefix?: string;
  renderListMeta?: (record: TSummary) => ReactNode;
}

function statusColor(status: string): ArtifactStatusColor {
  if (status.startsWith('Superseded') || status === 'Deprecated') {
    return 'warning';
  }
  if (status === 'Accepted') {
    return 'success';
  }
  if (status === 'Proposed') {
    return 'primary';
  }
  return 'default';
}

function artifactNumber(record: ArtifactSummaryBase, prefix?: string) {
  return prefix ? `${prefix}-${record.num}` : record.num;
}

function recordUuid(record: ArtifactSummaryBase) {
  return record.iri.slice(Math.max(record.iri.lastIndexOf('/'), record.iri.lastIndexOf('#')) + 1);
}

/**
 * Builds the client-side search haystack from the complete rendered artifact and
 * its list metadata. Keeping this pure makes filtering consistent across every
 * generated-artifact page without coupling it to an artifact-specific response.
 */
function artifactSearchText(record: ArtifactSummaryBase, prefix?: string) {
  return [
    artifactNumber(record, prefix),
    record.title,
    record.status,
    record.date,
    record.author,
    record.iri,
    record.search_text,
  ]
    .join('\n')
    .toLocaleLowerCase();
}

function ArtifactListItem<TSummary extends ArtifactSummaryBase>({
  record,
  prefix,
  selected,
  onSelect,
  renderListMeta,
}: {
  record: TSummary;
  prefix?: string;
  selected: boolean;
  onSelect: () => void;
  renderListMeta?: (record: TSummary) => ReactNode;
}) {
  return (
    <ListItemButton
      selected={selected}
      onClick={onSelect}
      sx={{
        alignItems: 'flex-start',
        borderBottom: 1,
        borderColor: 'divider',
        py: 1,
        gap: 1,
      }}
    >
      <Box sx={{ minWidth: prefix ? 68 : 52, pt: 0.25 }}>
        <Typography variant="caption" color="text.secondary">
          {artifactNumber(record, prefix)}
        </Typography>
      </Box>
      <Box sx={{ minWidth: 0, flex: 1 }}>
        <Typography
          variant="body2"
          sx={{ fontWeight: 650, overflow: 'hidden', textOverflow: 'ellipsis' }}
          title={record.title}
        >
          {record.title}
        </Typography>
        <Stack direction="row" spacing={0.75} alignItems="center" sx={{ mt: 0.75, minWidth: 0 }}>
          <Chip size="small" color={statusColor(record.status)} label={record.status} sx={{ maxWidth: 180 }} />
          {renderListMeta?.(record)}
          <Typography variant="caption" color="text.secondary">
            {record.date}
          </Typography>
        </Stack>
      </Box>
    </ListItemButton>
  );
}

export default function GeneratedArtifactPage<TSummary extends ArtifactSummaryBase, TList, TWarnings>({
  targetUuid,
  onNavigateArtifact,
  onNavigateRecord,
  artifactKind,
  title,
  emptyText,
  selectText,
  refreshTooltip,
  downloadTooltip,
  archiveFilename,
  sidebarMinWidth,
  sidebarMaxWidth,
  loadList,
  loadDetail,
  downloadArchive,
  recordsOf,
  generatedFileCount,
  warningsOf,
  warningCount,
  renderWarningSummary,
  recordPrefix,
  renderListMeta,
}: GeneratedArtifactPageProps<TSummary, TList, TWarnings>) {
  const [list, setList] = useState<TList | null>(null);
  const [detail, setDetail] = useState<ArtifactDetail<TSummary> | null>(null);
  const [selectedNum, setSelectedNum] = useState<string | null>(null);
  const [loadingList, setLoadingList] = useState(false);
  const [loadingDetail, setLoadingDetail] = useState(false);
  const [downloading, setDownloading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');

  const records = useMemo(() => (list ? recordsOf(list) : []), [list, recordsOf]);
  const normalizedSearchQuery = searchQuery.trim().toLocaleLowerCase();
  // Filtering must not alter selection: a selected record may remain open while
  // the user searches the sidebar for another artifact.
  const filteredRecords = useMemo(
    () =>
      normalizedSearchQuery
        ? records.filter((record) =>
            artifactSearchText(record, recordPrefix).includes(normalizedSearchQuery),
          )
        : records,
    [normalizedSearchQuery, recordPrefix, records],
  );
  const selectedFromList = useMemo(
    () => records.find((record) => record.num === selectedNum) ?? null,
    [records, selectedNum],
  );

  const refreshList = async () => {
    setLoadingList(true);
    setError(null);
    try {
      const response = await loadList();
      const responseRecords = recordsOf(response);
      const target = targetUuid
        ? responseRecords.find((record) => recordUuid(record) === targetUuid)
        : null;
      setList(response);
      if (targetUuid) {
        setSelectedNum(target?.num ?? null);
        if (!target) {
          setError(`Record ${targetUuid} was not found.`);
        }
      } else {
        setSelectedNum((current) => current ?? responseRecords[0]?.num ?? null);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoadingList(false);
    }
  };

  const saveArchive = async () => {
    setDownloading(true);
    setError(null);
    try {
      const blob = await downloadArchive();
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement('a');
      anchor.href = url;
      anchor.download = archiveFilename;
      document.body.appendChild(anchor);
      anchor.click();
      anchor.remove();
      URL.revokeObjectURL(url);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setDownloading(false);
    }
  };

  useEffect(() => {
    refreshList();
  }, []);

  useEffect(() => {
    if (!targetUuid) {
      return;
    }
    const target = records.find((record) => recordUuid(record) === targetUuid);
    setSelectedNum(target?.num ?? null);
    if (records.length > 0) {
      setError(target ? null : `Record ${targetUuid} was not found.`);
    }
  }, [targetUuid, records]);

  useEffect(() => {
    if (!selectedNum) {
      setDetail(null);
      return;
    }

    let cancelled = false;
    setLoadingDetail(true);
    setError(null);
    loadDetail(selectedNum)
      .then((response) => {
        if (!cancelled) {
          setDetail(response);
        }
      })
      .catch((err) => {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err));
        }
      })
      .finally(() => {
        if (!cancelled) {
          setLoadingDetail(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [selectedNum]);

  const warnings = list ? warningsOf(list) : null;

  return (
    <Box sx={{ height: '100%', display: 'flex', overflow: 'hidden', bgcolor: 'background.default' }}>
      <Box
        sx={{
          width: '38%',
          minWidth: sidebarMinWidth,
          maxWidth: sidebarMaxWidth,
          display: 'flex',
          flexDirection: 'column',
          borderRight: 1,
          borderColor: 'divider',
          bgcolor: 'background.paper',
        }}
      >
        <Box sx={{ p: 1.5, borderBottom: 1, borderColor: 'divider' }}>
          <Stack direction="row" spacing={1} alignItems="center" justifyContent="space-between">
            <Box sx={{ minWidth: 0 }}>
              <Typography variant="h6">{title}</Typography>
              <Typography variant="caption" color="text.secondary">
                {list ? `${generatedFileCount(list)} generated files` : 'Generated files'}
              </Typography>
            </Box>
            <Stack direction="row" spacing={1}>
              <Tooltip title={refreshTooltip}>
                <span>
                  <Button
                    size="small"
                    variant="outlined"
                    startIcon={loadingList ? <CircularProgress color="inherit" size={16} /> : <RefreshIcon />}
                    disabled={loadingList}
                    onClick={refreshList}
                  >
                    Refresh
                  </Button>
                </span>
              </Tooltip>
              <Tooltip title={downloadTooltip}>
                <span>
                  <Button
                    size="small"
                    variant="contained"
                    startIcon={downloading ? <CircularProgress color="inherit" size={16} /> : <DownloadIcon />}
                    disabled={downloading || !list}
                    onClick={saveArchive}
                  >
                    ZIP
                  </Button>
                </span>
              </Tooltip>
            </Stack>
          </Stack>
          <TextField
            fullWidth
            type="search"
            size="small"
            label="Search records"
            placeholder="Search title or content"
            value={searchQuery}
            onChange={(event) => setSearchQuery(event.target.value)}
            sx={{ mt: 1.5 }}
          />
        </Box>
        {warnings && warningCount(warnings) > 0 && <Box sx={{ p: 1 }}>{renderWarningSummary(warnings)}</Box>}
        <Box sx={{ flex: 1, minHeight: 0, overflow: 'auto' }}>
          {loadingList && !list ? (
            <Box sx={{ height: '100%', display: 'grid', placeItems: 'center' }}>
              <CircularProgress size={22} />
            </Box>
          ) : filteredRecords.length ? (
            <List disablePadding>
              {filteredRecords.map((record) => (
                <ArtifactListItem
                  key={record.num}
                  record={record}
                  prefix={recordPrefix}
                  selected={record.num === selectedNum}
                  onSelect={() => {
                    setSelectedNum(record.num);
                    onNavigateArtifact?.({ kind: artifactKind, iri: record.iri });
                  }}
                  renderListMeta={renderListMeta}
                />
              ))}
            </List>
          ) : (
            <Box sx={{ p: 2 }}>
              <Typography variant="body2" color="text.secondary">
                {records.length ? `No records match “${searchQuery.trim()}”.` : emptyText}
              </Typography>
            </Box>
          )}
        </Box>
      </Box>
      <Box sx={{ flex: 1, minWidth: 0, display: 'flex', flexDirection: 'column' }}>
        {error && (
          <Alert severity="error" onClose={() => setError(null)} sx={{ m: 1 }}>
            {error}
          </Alert>
        )}
        {selectedFromList && (
          <>
            <Box sx={{ px: 2, py: 1.25, borderBottom: 1, borderColor: 'divider', bgcolor: 'background.paper' }}>
              <Stack direction="row" spacing={1} alignItems="center" sx={{ minWidth: 0 }}>
                <Typography variant="h6" sx={{ minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis' }}>
                  {artifactNumber(selectedFromList, recordPrefix)}. {selectedFromList.title}
                </Typography>
                <Chip size="small" color={statusColor(selectedFromList.status)} label={selectedFromList.status} />
              </Stack>
              <Typography
                variant="caption"
                color="text.secondary"
                title={selectedFromList.iri}
                sx={{ display: 'block', overflow: 'hidden', textOverflow: 'ellipsis' }}
              >
                {selectedFromList.date} - {selectedFromList.author} - {selectedFromList.iri}
              </Typography>
            </Box>
            <Divider />
          </>
        )}
        <Box sx={{ flex: 1, minHeight: 0, overflow: 'auto', p: 2 }}>
          {loadingDetail ? (
            <Box sx={{ height: '100%', display: 'grid', placeItems: 'center' }}>
              <CircularProgress size={22} />
            </Box>
          ) : detail ? (
            <Box sx={{ maxWidth: 1100, mx: 'auto' }}>
              <Typography variant="subtitle1" sx={{ fontWeight: 650, mb: 1 }}>
                Connections
              </Typography>
              <RecordNeighborhoodGraph
                uuid={recordUuid(detail.summary)}
                onNavigateRecord={onNavigateRecord}
              />
              <Divider sx={{ my: 2 }} />
              <Box
                sx={{
                  maxWidth: 920,
                  mx: 'auto',
                  overflowWrap: 'anywhere',
                  '& h1': { fontSize: 24, mt: 0 },
                  '& h2': { fontSize: 18, mt: 3 },
                  '& p, & li': { fontSize: 14, lineHeight: 1.65 },
                  '& code': {
                    px: 0.5,
                    py: 0.15,
                    borderRadius: 0.5,
                    bgcolor: 'action.hover',
                    fontSize: '0.9em',
                  },
                }}
              >
                <LinkedMarkdown markdown={detail.markdown} onNavigateArtifact={onNavigateArtifact} />
              </Box>
            </Box>
          ) : (
            <Typography variant="body2" color="text.secondary">
              {selectText}
            </Typography>
          )}
        </Box>
      </Box>
    </Box>
  );
}
