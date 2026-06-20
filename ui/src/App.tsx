import { useEffect, useState } from 'react';
import { Alert, Box, CircularProgress, Typography } from '@mui/material';
import ChatIcon from '@mui/icons-material/Forum';
import QueryStatsIcon from '@mui/icons-material/QueryStats';
import AppShell, { PageKey } from './components/layout/AppShell';
import ChatPage from './pages/ChatPage';
import SparqlPage from './pages/SparqlPage';
import { api } from './api/client';
import { HealthResponse } from './api/types';
import { MooseThemeMode } from './styles/theme';

interface AppProps {
  themeMode: MooseThemeMode;
  onToggleThemeMode: () => void;
}

export default function App({ themeMode, onToggleThemeMode }: AppProps) {
  const [page, setPage] = useState<PageKey>('chat');
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .health()
      .then(setHealth)
      .catch((err) => setError(err instanceof Error ? err.message : String(err)));
  }, []);

  const nav = [
    { key: 'chat' as const, label: 'Chat', icon: <ChatIcon fontSize="small" /> },
    { key: 'sparql' as const, label: 'SPARQL', icon: <QueryStatsIcon fontSize="small" /> },
  ];

  return (
    <AppShell
      page={page}
      onPageChange={setPage}
      nav={nav}
      health={health}
      themeMode={themeMode}
      onToggleThemeMode={onToggleThemeMode}
    >
      {error && (
        <Alert severity="error" sx={{ m: 2 }}>
          {error}
        </Alert>
      )}
      {!health && !error ? (
        <Box sx={{ height: '100%', display: 'grid', placeItems: 'center' }}>
          <Box sx={{ display: 'flex', gap: 1, alignItems: 'center' }}>
            <CircularProgress size={18} />
            <Typography variant="body2" color="text.secondary">
              Connecting
            </Typography>
          </Box>
        </Box>
      ) : page === 'chat' ? (
        <ChatPage />
      ) : (
        <SparqlPage />
      )}
    </AppShell>
  );
}
