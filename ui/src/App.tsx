import { useEffect, useRef, useState } from 'react';
import { Alert, Box, CircularProgress, Typography } from '@mui/material';
import ArticleIcon from '@mui/icons-material/Article';
import AssignmentTurnedInIcon from '@mui/icons-material/AssignmentTurnedIn';
import ChatIcon from '@mui/icons-material/Forum';
import GavelIcon from '@mui/icons-material/Gavel';
import ImportExportIcon from '@mui/icons-material/ImportExport';
import InsightsIcon from '@mui/icons-material/Insights';
import MoveToInboxIcon from '@mui/icons-material/MoveToInbox';
import QueryStatsIcon from '@mui/icons-material/QueryStats';
import SchoolIcon from '@mui/icons-material/School';
import AutoStoriesIcon from '@mui/icons-material/AutoStories';
import AppShell, { PageKey } from './components/layout/AppShell';
import AdrsPage from './pages/AdrsPage';
import ChatPage from './pages/ChatPage';
import ConstraintsPage from './pages/ConstraintsPage';
import DebtPage from './pages/DebtPage';
import GraphTransferPage from './pages/GraphTransferPage';
import RatificationsPage from './pages/RatificationsPage';
import LessonsPage from './pages/LessonsPage';
import RequirementsPage from './pages/RequirementsPage';
import RecordPage from './pages/RecordPage';
import SparqlPage from './pages/SparqlPage';
import StoriesPage from './pages/StoriesPage';
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

export function confirmStoryNavigation(
  hasUnsavedChanges: boolean,
  confirmDiscard: () => boolean = () => window.confirm('Discard your unsaved Story changes?'),
): boolean {
  return !hasUnsavedChanges || confirmDiscard();
}

export function confirmStoryHashNavigation(
  hasUnsavedChanges: boolean,
  nextRoute: RecordRoute | null,
  restoreAcceptedLocation: () => void,
  confirmDiscard?: () => boolean,
): boolean {
  if (!nextRoute || confirmStoryNavigation(hasUnsavedChanges, confirmDiscard)) {
    return true;
  }
  restoreAcceptedLocation();
  return false;
}

export interface RecordRoute {
  kind: ArtifactKind | 'record';
  uuid: string;
}

export function pageNavigationIsNoop(
  currentPage: PageKey,
  nextPage: PageKey,
  activeRoute: RecordRoute | null,
): boolean {
  return currentPage === nextPage && activeRoute === null;
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

export function recordRouteForPage(iri: string, page: PageKey): RecordRoute | null {
  const route = recordRouteForIri(iri);
  if (!route || page !== 'stories') {
    return route;
  }
  // Evidence inspection is a transient view inside the Story workspace. Keep
  // even typed artifacts on the generic record route so StoriesPage stays
  // mounted with its quiz and curation state intact.
  return { kind: 'record', uuid: route.uuid };
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
  const [storyComponentIri, setStoryComponentIri] = useState<string | null>(null);
  const [storyDirty, setStoryDirty] = useState(false);
  const storyDirtyRef = useRef(storyDirty);
  const acceptedHashRef = useRef(window.location.hash);
  storyDirtyRef.current = storyDirty;

  useEffect(() => {
    if (!storyDirty) return;
    const preventUnload = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = '';
    };
    window.addEventListener('beforeunload', preventUnload);
    return () => window.removeEventListener('beforeunload', preventUnload);
  }, [storyDirty]);

  useEffect(() => {
    api
      .health()
      .then(setHealth)
      .catch((err) => setError(err instanceof Error ? err.message : String(err)));
  }, []);

  useEffect(() => {
    const syncRecordHash = () => {
      const route = recordRouteFromHash(window.location.hash);
      if (
        !confirmStoryHashNavigation(storyDirtyRef.current, route, () => {
          const acceptedUrl = `${window.location.pathname}${window.location.search}${acceptedHashRef.current}`;
          window.history.pushState(null, '', acceptedUrl);
        })
      ) {
        return;
      }
      acceptedHashRef.current = window.location.hash;
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
    { key: 'stories' as const, label: 'Stories', icon: <AutoStoriesIcon fontSize="small" /> },
    { key: 'debt' as const, label: 'Debt', icon: <InsightsIcon fontSize="small" /> },
    {
      key: 'ratifications' as const,
      label: 'Ratifications',
      icon: <MoveToInboxIcon fontSize="small" />,
    },
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
    const route = recordRouteForPage(iri, page);
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
    if (pageNavigationIsNoop(page, nextPage, recordRoute)) return;
    if (!confirmStoryNavigation(storyDirty)) {
      return;
    }
    if (window.location.hash) {
      window.location.hash = '';
    }
    setStoryDirty(false);
    setRecordRoute(null);
    setStoryComponentIri(null);
    setPage(nextPage);
  };

  const navigateStory = (componentIri: string) => {
    if (window.location.hash) {
      window.location.hash = '';
    }
    setRecordRoute(null);
    setStoryComponentIri(componentIri);
    setPage('stories');
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
      ) : page === 'stories' ? (
        <Box sx={{ height: '100%' }}>
          <Box sx={{ display: recordRoute?.kind === 'record' ? 'none' : 'block', height: '100%' }}>
            <StoriesPage
              onNavigateRecord={navigateRecord}
              initialComponentIri={storyComponentIri}
              onDirtyChange={setStoryDirty}
            />
          </Box>
          {recordRoute?.kind === 'record' && (
            <RecordPage
              uuid={recordRoute.uuid}
              onNavigateArtifact={(target) => navigateRecord(target.iri)}
              onNavigateRecord={navigateRecord}
              onTellStory={navigateStory}
              resolveArtifacts={false}
            />
          )}
        </Box>
      ) : recordRoute?.kind === 'record' ? (
        <RecordPage
          uuid={recordRoute.uuid}
          onNavigateArtifact={navigateArtifact}
          onNavigateRecord={navigateRecord}
          onResolveArtifact={replaceLegacyRecordRoute}
          onTellStory={navigateStory}
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
      ) : page === 'debt' ? (
        <DebtPage onNavigateRecord={navigateRecord} onTellStory={navigateStory} />
      ) : page === 'ratifications' ? (
        <RatificationsPage onNavigateRecord={navigateRecord} />
      ) : page === 'sparql' ? (
        <SparqlPage />
      ) : (
        <GraphTransferPage />
      )}
    </AppShell>
  );
}
