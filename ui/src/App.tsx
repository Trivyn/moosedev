import { useEffect, useState } from 'react';
import { Alert, Box, CircularProgress, Typography } from '@mui/material';
import ArticleIcon from '@mui/icons-material/Article';
import AssignmentTurnedInIcon from '@mui/icons-material/AssignmentTurnedIn';
import ChatIcon from '@mui/icons-material/Forum';
import ImportExportIcon from '@mui/icons-material/ImportExport';
import QueryStatsIcon from '@mui/icons-material/QueryStats';
import SchoolIcon from '@mui/icons-material/School';
import AppShell, { PageKey } from './components/layout/AppShell';
import AdrsPage from './pages/AdrsPage';
import ChatPage from './pages/ChatPage';
import GraphTransferPage from './pages/GraphTransferPage';
import LessonsPage from './pages/LessonsPage';
import RequirementsPage from './pages/RequirementsPage';
import SparqlPage from './pages/SparqlPage';
import { api } from './api/client';
import { HealthResponse } from './api/types';
import { ArtifactTarget } from './components/artifacts/LinkedMarkdown';
import { MooseThemeMode } from './styles/theme';

interface AppProps {
  themeMode: MooseThemeMode;
  onToggleThemeMode: () => void;
}

export default function App({ themeMode, onToggleThemeMode }: AppProps) {
  const [page, setPage] = useState<PageKey>('chat');
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [artifactTargets, setArtifactTargets] = useState<Partial<Record<PageKey, string>>>({});

  useEffect(() => {
    api
      .health()
      .then(setHealth)
      .catch((err) => setError(err instanceof Error ? err.message : String(err)));
  }, []);

  const nav = [
    { key: 'chat' as const, label: 'Chat', icon: <ChatIcon fontSize="small" /> },
    { key: 'adrs' as const, label: 'ADRs', icon: <ArticleIcon fontSize="small" /> },
    {
      key: 'requirements' as const,
      label: 'Requirements',
      icon: <AssignmentTurnedInIcon fontSize="small" />,
    },
    { key: 'lessons' as const, label: 'Lessons', icon: <SchoolIcon fontSize="small" /> },
    { key: 'sparql' as const, label: 'SPARQL', icon: <QueryStatsIcon fontSize="small" /> },
    { key: 'transfer' as const, label: 'Import / Export', icon: <ImportExportIcon fontSize="small" /> },
  ];

  const navigateArtifact = (target: ArtifactTarget) => {
    setArtifactTargets((current) => ({ ...current, [target.kind]: target.iri }));
    setPage(target.kind);
  };

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
      ) : page === 'adrs' ? (
        <AdrsPage targetIri={artifactTargets.adrs} onNavigateArtifact={navigateArtifact} />
      ) : page === 'requirements' ? (
        <RequirementsPage
          targetIri={artifactTargets.requirements}
          onNavigateArtifact={navigateArtifact}
        />
      ) : page === 'lessons' ? (
        <LessonsPage targetIri={artifactTargets.lessons} onNavigateArtifact={navigateArtifact} />
      ) : page === 'sparql' ? (
        <SparqlPage />
      ) : (
        <GraphTransferPage />
      )}
    </AppShell>
  );
}
