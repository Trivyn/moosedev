import { Box, Table, TableBody, TableCell, TableContainer, TableHead, TableRow, Typography } from '@mui/material';
import { QueryBinding, QueryResponse, QueryValue } from '../../api/types';
import { shortName } from '../graph/graphUtils';

interface ResultsTableProps {
  result: QueryResponse | null;
}

function displayValue(value?: QueryValue): string {
  if (!value) return '';
  if (value.type === 'uri') return shortName(value.value);
  if (value.lang) return `${value.value}@${value.lang}`;
  return value.value;
}

export default function ResultsTable({ result }: ResultsTableProps) {
  if (!result) {
    return (
      <Box sx={{ height: '100%', display: 'grid', placeItems: 'center', color: 'text.secondary' }}>
        <Typography variant="body2">Execute a query to see results.</Typography>
      </Box>
    );
  }
  if (result.query_type === 'ASK') {
    return (
      <Box sx={{ p: 2 }}>
        <Typography variant="h6">{result.boolean ? 'True' : 'False'}</Typography>
      </Box>
    );
  }

  const vars = result.head?.vars ?? (result.triples ? ['subject', 'predicate', 'object'] : []);
  const rows: QueryBinding[] =
    result.results?.bindings ??
    result.triples?.map((triple) => ({
      subject: triple.subject,
      predicate: triple.predicate,
      object: triple.object,
    })) ??
    [];

  return (
    <TableContainer sx={{ height: '100%' }}>
      <Table stickyHeader size="small">
        <TableHead>
          <TableRow>
            {vars.map((name) => (
              <TableCell key={name}>{name}</TableCell>
            ))}
          </TableRow>
        </TableHead>
        <TableBody>
          {rows.map((row, index) => (
            <TableRow key={index} hover>
              {vars.map((name) => (
                <TableCell
                  key={name}
                  title={row[name]?.value ?? ''}
                  sx={{ maxWidth: 360, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
                >
                  {displayValue(row[name])}
                </TableCell>
              ))}
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </TableContainer>
  );
}
