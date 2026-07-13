import { useEffect, useMemo, useState } from 'react';
import {
  alpha,
  Alert,
  Box,
  Chip,
  CircularProgress,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableContainer,
  TableHead,
  TableRow,
  Tooltip,
  Typography,
} from '@mui/material';
import { api } from '../api/client';
import { ComponentCoverage, WhyCoverageResponse } from '../api/types';

interface DebtPageProps {
  onNavigateRecord: (iri: string) => void;
}

/** 0/0 (no public surface) sorts as fully covered — there is nothing to document. */
function coverageValue(component: ComponentCoverage): number {
  return component.coverage ?? 1;
}

function barColor(ratio: number): string {
  const hue = Math.round(ratio * 120); // 0 = red, 120 = green
  return `hsl(${hue}, 62%, 45%)`;
}

function CoverageBar({ ratio }: { ratio: number }) {
  const pct = Math.round(ratio * 100);
  return (
    <Stack direction="row" alignItems="center" spacing={1}>
      <Box
        sx={(theme) => ({
          flex: 1,
          height: 8,
          borderRadius: 1,
          bgcolor: alpha(theme.palette.text.primary, 0.08),
          overflow: 'hidden',
        })}
      >
        <Box sx={{ width: `${pct}%`, height: '100%', bgcolor: barColor(ratio) }} />
      </Box>
      <Typography variant="caption" sx={{ minWidth: 34, textAlign: 'right' }}>
        {pct}%
      </Typography>
    </Stack>
  );
}

export default function DebtPage({ onNavigateRecord }: DebtPageProps) {
  const [data, setData] = useState<WhyCoverageResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .debt()
      .then(setData)
      .catch((err) => setError(err instanceof Error ? err.message : String(err)));
  }, []);

  const rows = useMemo(
    () => (data ? [...data.components].sort((a, b) => coverageValue(a) - coverageValue(b)) : []),
    [data],
  );

  if (error) {
    return (
      <Alert severity="error" sx={{ m: 2 }}>
        {error}
      </Alert>
    );
  }
  if (!data) {
    return (
      <Box sx={{ height: '100%', display: 'grid', placeItems: 'center' }}>
        <CircularProgress size={18} />
      </Box>
    );
  }

  return (
    <Box sx={{ p: 3, height: '100%', overflow: 'auto' }}>
      <Typography variant="h5" gutterBottom>
        Why-coverage
      </Typography>
      <Typography variant="body2" color="text.secondary" sx={{ mb: 2 }}>
        The documented fraction of each component&apos;s public code surface — the public entities
        carrying at least one linked rationale record. Lower coverage is more comprehension debt.
        {data.unmapped > 0 &&
          ` ${data.unmapped} public definition${data.unmapped === 1 ? '' : 's'} map to no component.`}
      </Typography>
      <TableContainer>
        <Table stickyHeader size="small">
          <TableHead>
            <TableRow>
              <TableCell>Component</TableCell>
              <TableCell align="right">Documented</TableCell>
              <TableCell sx={{ width: '45%' }}>Coverage</TableCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {rows.map((component) => {
              const empty = component.denominator === 0;
              const documented = `${component.numerator} / ${component.denominator}`;
              const cell = empty ? (
                <Chip size="small" variant="outlined" label="no public surface" />
              ) : component.undocumented.length > 0 ? (
                <Tooltip
                  title={`Undocumented: ${component.undocumented.slice(0, 20).join(', ')}${
                    component.undocumented.length > 20 ? ', …' : ''
                  }`}
                >
                  <span>{documented}</span>
                </Tooltip>
              ) : (
                <span>{documented}</span>
              );
              return (
                <TableRow
                  key={component.iri ?? component.name}
                  hover
                  sx={{ cursor: component.iri ? 'pointer' : 'default' }}
                  onClick={() => component.iri && onNavigateRecord(component.iri)}
                >
                  <TableCell>{component.name}</TableCell>
                  <TableCell align="right">{cell}</TableCell>
                  <TableCell>
                    {empty ? '—' : <CoverageBar ratio={coverageValue(component)} />}
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </TableContainer>
    </Box>
  );
}
