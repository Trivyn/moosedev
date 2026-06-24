import { ChangeEvent, useState } from 'react';
import {
  Alert,
  Box,
  Button,
  CircularProgress,
  Divider,
  MenuItem,
  Stack,
  TextField,
  Typography,
} from '@mui/material';
import DownloadIcon from '@mui/icons-material/Download';
import RestoreIcon from '@mui/icons-material/Restore';
import UploadFileIcon from '@mui/icons-material/UploadFile';
import { api } from '../api/client';
import { GraphImportResponse } from '../api/types';

const FORMAT_LABELS: Record<string, string> = {
  nq: 'N-Quads',
  ttl: 'Turtle',
  nt: 'N-Triples',
};
const EXPORT_FORMATS = ['nq', 'ttl', 'nt'];
const IMPORT_FORMATS = ['ttl', 'nt', 'nq'];

function importExtension(format: string) {
  return format === 'nt' ? 'nt' : format === 'nq' ? 'nq' : 'ttl';
}

export default function GraphTransferPage() {
  const [exportFormat, setExportFormat] = useState('nq');
  const [exportGraph, setExportGraph] = useState('project');
  const [exportLoading, setExportLoading] = useState(false);

  const [importFormat, setImportFormat] = useState('ttl');
  const [importGraph, setImportGraph] = useState('project');
  const [importMode, setImportMode] = useState('patch');
  const [importText, setImportText] = useState('');
  const [restoreConfirm, setRestoreConfirm] = useState('');
  const [importLoading, setImportLoading] = useState(false);
  const [importResult, setImportResult] = useState<GraphImportResponse | null>(null);
  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const restoreBlocked = importMode === 'replace' && restoreConfirm !== 'RESTORE';
  const importDisabled = importLoading || !importText.trim() || restoreBlocked;
  const importGraphs = [
    { value: 'project', label: 'Project' },
    { value: 'provenance', label: 'Provenance' },
    { value: 'all', label: 'All', disabled: importFormat !== 'nq' },
  ];

  const downloadGraph = async () => {
    setExportLoading(true);
    setError(null);
    try {
      const blob = await api.exportGraph({ format: exportFormat, graph: exportGraph });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement('a');
      anchor.href = url;
      anchor.download = `moosedev-${exportGraph}.${exportFormat}`;
      document.body.appendChild(anchor);
      anchor.click();
      anchor.remove();
      URL.revokeObjectURL(url);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setExportLoading(false);
    }
  };

  const readFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (!file) {
      return;
    }
    setError(null);
    setSelectedFile(file.name);
    setImportText(await file.text());
    event.target.value = '';
  };

  const submitImport = async () => {
    setImportLoading(true);
    setError(null);
    setImportResult(null);
    try {
      const result = await api.importGraph(
        { format: importFormat, graph: importGraph, mode: importMode },
        importText,
      );
      setImportResult(result);
      setRestoreConfirm('');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setImportLoading(false);
    }
  };

  const changeImportFormat = (format: string) => {
    setImportFormat(format);
    if (format !== 'nq' && importGraph === 'all') {
      setImportGraph('project');
    }
  };

  return (
    <Box sx={{ height: '100%', overflow: 'auto', bgcolor: 'background.default' }}>
      <Box sx={{ maxWidth: 980, mx: 'auto', p: 2, display: 'flex', flexDirection: 'column', gap: 2 }}>
        <Box>
          <Typography variant="h6">Import / Export</Typography>
          <Typography variant="caption" color="text.secondary">
            Project graph: https://moosedev.dev/kg/project
          </Typography>
        </Box>

        {error && (
          <Alert severity="error" onClose={() => setError(null)}>
            {error}
          </Alert>
        )}

        <Box sx={{ border: 1, borderColor: 'divider', borderRadius: 1, bgcolor: 'background.paper' }}>
          <Box sx={{ p: 1.5 }}>
            <Typography variant="subtitle1">Export</Typography>
          </Box>
          <Divider />
          <Stack direction={{ xs: 'column', sm: 'row' }} spacing={1.5} sx={{ p: 1.5 }} alignItems="flex-start">
            <TextField
              select
              size="small"
              label="Format"
              value={exportFormat}
              onChange={(event) => setExportFormat(event.target.value)}
              sx={{ minWidth: 150 }}
            >
              {EXPORT_FORMATS.map((format) => (
                <MenuItem key={format} value={format}>
                  {FORMAT_LABELS[format]}
                </MenuItem>
              ))}
            </TextField>
            <TextField
              select
              size="small"
              label="Graph"
              value={exportGraph}
              onChange={(event) => setExportGraph(event.target.value)}
              sx={{ minWidth: 150 }}
            >
              <MenuItem value="project">Project</MenuItem>
              <MenuItem value="provenance">Provenance</MenuItem>
              <MenuItem value="all">All</MenuItem>
            </TextField>
            <Button
              variant="contained"
              startIcon={exportLoading ? <CircularProgress color="inherit" size={16} /> : <DownloadIcon />}
              disabled={exportLoading}
              onClick={downloadGraph}
            >
              Download
            </Button>
          </Stack>
        </Box>

        <Box sx={{ border: 1, borderColor: 'divider', borderRadius: 1, bgcolor: 'background.paper' }}>
          <Box sx={{ p: 1.5 }}>
            <Typography variant="subtitle1">Import</Typography>
          </Box>
          <Divider />
          <Stack direction={{ xs: 'column', md: 'row' }} spacing={1.5} sx={{ p: 1.5 }} alignItems="flex-start">
            <TextField
              select
              size="small"
              label="Format"
              value={importFormat}
              onChange={(event) => changeImportFormat(event.target.value)}
              sx={{ minWidth: 150 }}
            >
              {IMPORT_FORMATS.map((format) => (
                <MenuItem key={format} value={format}>
                  {FORMAT_LABELS[format]}
                </MenuItem>
              ))}
            </TextField>
            <TextField
              select
              size="small"
              label="Graph"
              value={importGraph}
              onChange={(event) => setImportGraph(event.target.value)}
              sx={{ minWidth: 150 }}
            >
              {importGraphs.map((item) => (
                <MenuItem key={item.value} value={item.value} disabled={item.disabled}>
                  {item.label}
                </MenuItem>
              ))}
            </TextField>
            <TextField
              select
              size="small"
              label="Mode"
              value={importMode}
              onChange={(event) => setImportMode(event.target.value)}
              sx={{ minWidth: 150 }}
            >
              <MenuItem value="patch">Patch</MenuItem>
              <MenuItem value="replace">Replace</MenuItem>
            </TextField>
            <Button component="label" variant="outlined" startIcon={<UploadFileIcon />}>
              File
              <input
                hidden
                type="file"
                accept={`.${importExtension(importFormat)},.ttl,.nt,.nq,text/turtle,application/n-triples,application/n-quads`}
                onChange={readFile}
              />
            </Button>
            <Button
              variant="contained"
              color={importMode === 'replace' ? 'warning' : 'primary'}
              startIcon={importLoading ? <CircularProgress color="inherit" size={16} /> : <RestoreIcon />}
              disabled={importDisabled}
              onClick={submitImport}
            >
              Import
            </Button>
          </Stack>
          {selectedFile && (
            <Typography variant="caption" color="text.secondary" sx={{ display: 'block', px: 1.5, pb: 1 }}>
              {selectedFile}
            </Typography>
          )}
          <Box sx={{ px: 1.5, pb: 1.5 }}>
            <TextField
              fullWidth
              multiline
              minRows={12}
              maxRows={18}
              label="RDF"
              value={importText}
              onChange={(event) => setImportText(event.target.value)}
              slotProps={{
                input: {
                  sx: { fontFamily: 'monospace', fontSize: 13 },
                },
              }}
            />
          </Box>
          {importMode === 'replace' && (
            <Box sx={{ px: 1.5, pb: 1.5 }}>
              <TextField
                size="small"
                label="Confirmation"
                value={restoreConfirm}
                onChange={(event) => setRestoreConfirm(event.target.value)}
                placeholder="RESTORE"
                sx={{ width: 220 }}
              />
            </Box>
          )}
          {importResult && (
            <Box sx={{ px: 1.5, pb: 1.5 }}>
              <Alert severity="success">
                Inserted {importResult.inserted_quad_count}, skipped {importResult.skipped_existing_count},
                removed {importResult.removed_quad_count}, duplicates {importResult.duplicate_input_count}.
              </Alert>
            </Box>
          )}
        </Box>
      </Box>
    </Box>
  );
}
