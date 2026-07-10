import { useEffect, useState } from 'react';
import { Alert, Box, CircularProgress, Typography } from '@mui/material';
import ArticleIcon from '@mui/icons-material/Article';
import AssignmentTurnedInIcon from '@mui/icons-material/AssignmentTurnedIn';
import ChatIcon from '@mui/icons-material/Forum';
import GavelIcon from '@mui/icons-material/Gavel';
import ImportExportIcon from '@mui/icons-material/ImportExport';
import QueryStatsIcon from '@mui/icons-material/QueryStats';
import SchoolIcon from '@mui/icons-material/School';
import AppShell, { PageKey } from './components/layout/AppShell';
import AdrsPage from './pages/AdrsPage';
import ChatPage from './pages/ChatPage';
import ConstraintsPage from './pages/ConstraintsPage';
import GraphTransferPage from './pages/GraphTransferPage';
import LessonsPage from './pages/LessonsPage';
import RequirementsPage from './pages/RequirementsPage';
import RecordPage from './pages/RecordPage';
import SparqlPage from './pages/SparqlPage';
import { api } from './api/client';
import { HealthResponse } from './api/types';
import {
  artifactTargetForIri,
  ArtifactKind,
  ArtifactTarget,
} from './components/artifacts/LinkedMarkdown';
import { MooseThemeMode } from './styles/theme';

interface AppProps {
  themeMode: MooseThemeMode;
  onToggleThemeMode: () => void;
}

export interface RecordRoute {
  kind: ArtifactKind | 'record';
  uuid: string;
}

type ArtifactRoute = RecordRoute & { kind: ArtifactKind };

export function recordRouteFromHash(hash: string): RecordRoute | null {
  const match = /^#\/(record|adrs|requirements|lessons|constraints)\/([^/]+)$/.exec(hash);
  if (!match) {
    return null;
  }
  try {
    const uuid = decodeURIComponent(match[2]);
    return uuid.includes('/')
      ? null
      : { kind: match[1] as RecordRoute['kind'], uuid };
  } catch {
    return null;
  }
}

export function recordUuidFromHash(hash: string): string | null {
  const route = recordRouteFromHash(hash);
  return route?.kind === 'record' ? route.uuid : null;
}

function uuidFromIri(iri: string): string | null {
  const uuid = iri.slice(Math.max(iri.lastIndexOf('/'), iri.lastIndexOf('#')) + 1);
  return uuid || null;
}

export function recordRouteForIri(iri: string): RecordRoute | null {
  const uuid = uuidFromIri(iri);
  if (!uuid) {
    return null;
  }
  const artifact = artifactTargetForIri(iri);
  if (artifact) {
    return { kind: artifact.kind, uuid };
  }
  return iri.startsWith('https://moosedev.dev/kg/') ? { kind: 'record', uuid } : null;
}

function routeForArtifact(target: ArtifactTarget): ArtifactRoute | null {
  const uuid = uuidFromIri(target.iri);
  return uuid ? { kind: target.kind, uuid } : null;
}

function hashForRoute(route: RecordRoute) {
  return `#/${route.kind}/${encodeURIComponent(route.uuid)}`;
}

export default function App({ themeMode, onToggleThemeMode }: AppProps) {
  const [page, setPage] = useState<PageKey>('chat');
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [recordRoute, setRecordRoute] = useState<RecordRoute | null>(() =>
    recordRouteFromHash(window.location.hash),
  );

  useEffect(() => {
    api
      .health()
      .then(setHealth)
      .catch((err) => setError(err instanceof Error ? err.message : String(err)));
  }, []);

  useEffect(() => {
    const syncRecordHash = () => {
      const route = recordRouteFromHash(window.location.hash);
      setRecordRoute(route);
      if (route && route.kind !== 'record') {
        setPage(route.kind);
      }
    };
    syncRecordHash();
    window.addEventListener('hashchange', syncRecordHash);
    return () => window.removeEventListener('hashchange', syncRecordHash);
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
    { key: 'constraints' as const, label: 'Constraints', icon: <GavelIcon fontSize="small" /> },
    { key: 'sparql' as const, label: 'SPARQL', icon: <QueryStatsIcon fontSize="small" /> },
    { key: 'transfer' as const, label: 'Import / Export', icon: <ImportExportIcon fontSize="small" /> },
  ];

  const navigateArtifact = (target: ArtifactTarget) => {
    const route = routeForArtifact(target);
    if (route) {
      window.location.hash = hashForRoute(route);
    }
  };

  const navigateRecord = (iri: string) => {
    const route = recordRouteForIri(iri);
    if (route) {
      window.location.hash = hashForRoute(route);
    }
  };

  const replaceLegacyRecordRoute = (target: ArtifactTarget) => {
    const route = routeForArtifact(target);
    if (!route) {
      return;
    }
    window.history.replaceState(null, '', hashForRoute(route));
    setRecordRoute(route);
    setPage(route.kind);
  };

  const navigatePage = (nextPage: PageKey) => {
    if (window.location.hash) {
      window.location.hash = '';
    }
    setRecordRoute(null);
    setPage(nextPage);
  };

  return (
    <AppShell
      page={page}
      onPageChange={navigatePage}
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
      ) : recordRoute?.kind === 'record' ? (
        <RecordPage
          uuid={recordRoute.uuid}
          onNavigateArtifact={navigateArtifact}
          onNavigateRecord={navigateRecord}
          onResolveArtifact={replaceLegacyRecordRoute}
        />
      ) : page === 'chat' ? (
        <ChatPage />
      ) : page === 'adrs' ? (
        <AdrsPage
          targetUuid={recordRoute?.kind === 'adrs' ? recordRoute.uuid : undefined}
          onNavigateArtifact={navigateArtifact}
          onNavigateRecord={navigateRecord}
        />
      ) : page === 'requirements' ? (
        <RequirementsPage
          targetUuid={recordRoute?.kind === 'requirements' ? recordRoute.uuid : undefined}
          onNavigateArtifact={navigateArtifact}
          onNavigateRecord={navigateRecord}
        />
      ) : page === 'lessons' ? (
        <LessonsPage
          targetUuid={recordRoute?.kind === 'lessons' ? recordRoute.uuid : undefined}
          onNavigateArtifact={navigateArtifact}
          onNavigateRecord={navigateRecord}
        />
      ) : page === 'constraints' ? (
        <ConstraintsPage
          targetUuid={recordRoute?.kind === 'constraints' ? recordRoute.uuid : undefined}
          onNavigateArtifact={navigateArtifact}
          onNavigateRecord={navigateRecord}
        />
      ) : page === 'sparql' ? (
        <SparqlPage />
      ) : (
        <GraphTransferPage />
      )}
    </AppShell>
  );
}
