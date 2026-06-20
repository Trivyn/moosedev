import { useMemo, useState } from 'react';
import { Alert, Box, Button, CircularProgress, MenuItem, Stack, Tab, Tabs, TextField, Typography } from '@mui/material';
import DownloadIcon from '@mui/icons-material/Download';
import PlayArrowIcon from '@mui/icons-material/PlayArrow';
import { api } from '../api/client';
import { QueryResponse } from '../api/types';
import CytoscapeGraph from '../components/graph/CytoscapeGraph';
import { queryToGraph } from '../components/graph/graphUtils';
import RawResults from '../components/sparql/RawResults';
import ResultsTable from '../components/sparql/ResultsTable';
import SparqlEditor from '../components/sparql/SparqlEditor';

const QUICK_QUERIES = [
  {
    label: 'Records',
    query: `PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?subject ?predicate ?object
WHERE {
  ?subject ?predicate ?object .
}
LIMIT 100`,
  },
  {
    label: 'Decisions',
    query: `PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?subject ?predicate ?object
WHERE {
  ?subject a ?kind ;
           ?predicate ?object .
  FILTER(CONTAINS(STR(?kind), "ArchitecturalDecision"))
}
LIMIT 100`,
  },
  {
    label: 'Graph',
    query: `CONSTRUCT { ?subject ?predicate ?object }
WHERE {
  ?subject ?predicate ?object .
  FILTER(isIRI(?object))
}
LIMIT 150`,
  },
];

export default function SparqlPage() {
  const [query, setQuery] = useState(QUICK_QUERIES[0].query);
  const [result, setResult] = useState<QueryResponse | null>(null);
  const [tab, setTab] = useState(0);
  const [loading, setLoading] = useState(false);
  const [exportLoading, setExportLoading] = useState(false);
  const [exportFormat, setExportFormat] = useState('nq');
  const [error, setError] = useState<string | null>(null);
  const graph = useMemo(() => queryToGraph(result), [result]);

  const execute = async () => {
    setLoading(true);
    setError(null);
    try {
      const response = await api.sparql(query);
      setResult(response);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  const downloadGraph = async () => {
    setExportLoading(true);
    setError(null);
    try {
      const blob = await api.exportGraph({ format: exportFormat, graph: 'project' });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement('a');
      anchor.href = url;
      anchor.download = `moosedev-project.${exportFormat}`;
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

  return (
    <Box sx={{ height: '100%', display: 'flex', overflow: 'hidden' }}>
      <Box sx={{ width: '42%', minWidth: 460, display: 'flex', flexDirection: 'column', borderRight: 1, borderColor: 'divider' }}>
        <Box sx={{ p: 1.5, borderBottom: 1, borderColor: 'divider' }}>
          <Typography variant="h6">SPARQL</Typography>
          <Typography variant="caption" color="text.secondary">
            Default graph: https://moosedev.dev/kg/project
          </Typography>
        </Box>
        <Stack direction="row" spacing={1} sx={{ p: 1, flexWrap: 'wrap' }}>
          {QUICK_QUERIES.map((item) => (
            <Button key={item.label} size="small" variant="outlined" onClick={() => setQuery(item.query)}>
              {item.label}
            </Button>
          ))}
        </Stack>
        <Box sx={{ flex: 1, minHeight: 0, p: 1 }}>
          <SparqlEditor value={query} onChange={setQuery} />
        </Box>
        {error && (
          <Alert severity="error" onClose={() => setError(null)} sx={{ mx: 1, mb: 1 }}>
            {error}
          </Alert>
        )}
        <Stack direction="row" spacing={1} alignItems="center" sx={{ p: 1, borderTop: 1, borderColor: 'divider' }}>
          <Button
            variant="contained"
            startIcon={loading ? <CircularProgress color="inherit" size={16} /> : <PlayArrowIcon />}
            disabled={loading || !query.trim()}
            onClick={execute}
          >
            Execute
          </Button>
          <TextField
            select
            size="small"
            label="Format"
            value={exportFormat}
            onChange={(event) => setExportFormat(event.target.value)}
            sx={{ width: 110 }}
          >
            <MenuItem value="nq">N-Quads</MenuItem>
            <MenuItem value="ttl">Turtle</MenuItem>
            <MenuItem value="nt">N-Triples</MenuItem>
          </TextField>
          <Button
            variant="outlined"
            startIcon={exportLoading ? <CircularProgress color="inherit" size={16} /> : <DownloadIcon />}
            disabled={exportLoading}
            onClick={downloadGraph}
          >
            Download
          </Button>
        </Stack>
      </Box>
      <Box sx={{ flex: 1, minWidth: 0, display: 'flex', flexDirection: 'column' }}>
        <Tabs value={tab} onChange={(_, value) => setTab(value)} sx={{ borderBottom: 1, borderColor: 'divider' }}>
          <Tab label="Table" />
          <Tab label="Graph" />
          <Tab label="Raw" />
        </Tabs>
        <Box sx={{ flex: 1, minHeight: 0 }}>
          {tab === 0 && <ResultsTable result={result} />}
          {tab === 1 && <CytoscapeGraph nodes={graph.nodes} edges={graph.edges} />}
          {tab === 2 && <RawResults value={result} />}
        </Box>
      </Box>
    </Box>
  );
}
