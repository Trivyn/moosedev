import { Box, Typography } from '@mui/material';

interface RawResultsProps {
  value: unknown;
}

export default function RawResults({ value }: RawResultsProps) {
  if (!value) {
    return (
      <Box sx={{ height: '100%', display: 'grid', placeItems: 'center', color: 'text.secondary' }}>
        <Typography variant="body2">Execute a query to see raw JSON.</Typography>
      </Box>
    );
  }
  return (
    <Box component="pre" sx={{ m: 0, p: 2, height: '100%', overflow: 'auto', fontSize: 12 }}>
      {JSON.stringify(value, null, 2)}
    </Box>
  );
}
