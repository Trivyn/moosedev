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
import { ArtifactTarget } from './components/artifacts/LinkedMarkdown';
import {
  confirmStoryHashNavigation,
  confirmStoryNavigation,
  hashForRoute,
  pageNavigationIsNoop,
  RecordRoute,
  recordRouteForIri,
  recordRouteFromHash,
  recordUuidFromHash,
  routeForArtifact,
  storyEntityHash,
  storyEntityUuidFromHash,
  storySubjectNeedsResolving,
} from './routing';
import { MooseThemeMode } from './styles/theme';

interface AppProps {
  themeMode: MooseThemeMode;
  onToggleThemeMode: () => void;
}

export default function App({ themeMode, onToggleThemeMode }: AppProps) {
  const [page, setPage] = useState<PageKey>('chat');
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [recordRoute, setRecordRoute] = useState<RecordRoute | null>(() =>
    recordRouteFromHash(window.location.hash),
  );
  const [storySubjectIri, setStorySubjectIri] = useState<string | null>(null);
  // Which Story deep-link uuid `storySubjectIri` was resolved from, so a repeat
  // visit to the same uuid is not re-resolved.
  const resolvedSubjectUuidRef = useRef<string | null>(null);
  const [storyEntityUuid, setStoryEntityUuid] = useState<string | null>(() =>
    storyEntityUuidFromHash(window.location.hash),
  );
  const [storyDirty, setStoryDirty] = useState(false);
  // Deep-link failures are route-local: folding them into the app-wide
  // health `error` left a banner that nothing could clear.
  const [routeError, setRouteError] = useState<string | null>(null);
  // The page a Story deep link was launched from, so Back can return there.
  const pageBeforeStoryRef = useRef<PageKey | null>(null);
  const storyDirtyRef = useRef(storyDirty);
  const acceptedHashRef = useRef(window.location.hash);
  // A traversal fires popstate AND hashchange for ONE navigation. Coalescing
  // by task (rather than by URL) suppresses the companion event without
  // swallowing a genuinely new navigation that happens to land on the same
  // URL — e.g. backing out of a Story to a hashless origin, where both
  // entries have an empty hash.
  const coalescingRef = useRef(false);
  // The once-installed hash listener reads the current dirty state through this ref.
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
      const storyEntity = storyEntityUuidFromHash(window.location.hash);
      // Backing out to a hashless origin unmounts StoriesPage, so it is a real
      // destination for guard purposes even though the hash is empty. Passing
      // null here would discard unsaved curation without asking.
      const leavingToOrigin =
        !route && !storyEntity ? pageBeforeStoryRef.current : null;
      if (
        !confirmStoryHashNavigation(
          storyDirtyRef.current,
          route ?? storyEntity ?? leavingToOrigin,
          () => {
            const acceptedUrl = `${window.location.pathname}${window.location.search}${acceptedHashRef.current}`;
            window.history.pushState(null, '', acceptedUrl);
          },
        )
      ) {
        return;
      }
      acceptedHashRef.current = window.location.hash;
      // A failed Story lookup's banner belongs to the route that failed. Once
      // navigation is accepted the destination is a different route, so leaving
      // it up would report that failure over unrelated content.
      setRouteError(null);
      setRecordRoute(route);
      setStoryEntityUuid(storyEntity);
      if (storyEntity) {
        setPage('stories');
      } else if (route && route.kind !== 'record') {
        setPage(route.kind);
      } else if (!route && pageBeforeStoryRef.current) {
        // Back out of a Story deep link returns to the page it was launched
        // from; without this the empty hash leaves the user stranded on
        // Stories with no way back to a hashless origin like Debt.
        setPage(pageBeforeStoryRef.current);
        pageBeforeStoryRef.current = null;
      }
    };
    const handleLocationChange = () => {
      if (coalescingRef.current) return;
      coalescingRef.current = true;
      window.setTimeout(() => {
        coalescingRef.current = false;
      }, 0);
      syncRecordHash();
    };
    syncRecordHash();
    window.addEventListener('hashchange', handleLocationChange);
    // Also listen for popstate: a Story hash that was REPLACED with the same
    // empty-hash URL as its origin traverses back without any hash change, so
    // `hashchange` alone would strand the user on Stories. The handler reads
    // the current location and is idempotent, so the overlap is harmless.
    window.addEventListener('popstate', handleLocationChange);
    return () => {
      window.removeEventListener('hashchange', handleLocationChange);
      window.removeEventListener('popstate', handleLocationChange);
    };
  }, []);

  // A Story deep link names a record UUID; the daemon resolves which entity
  // that is, so the browser never has to reconstruct an IRI. The Story is then
  // told about THAT exact entity.
  useEffect(() => {
    if (!storySubjectNeedsResolving(storyEntityUuid, resolvedSubjectUuidRef.current, storySubjectIri))
      return;
    let cancelled = false;
    // Drop the previous subject BEFORE resolving: if this lookup fails, the URL
    // must not keep naming entity B while entity A's Story is still on screen.
    setStorySubjectIri(null);
    setRouteError(null);
    api
      .record(storyEntityUuid)
      .then((record) => {
        if (!cancelled) {
          resolvedSubjectUuidRef.current = storyEntityUuid;
          setStorySubjectIri(record.iri);
        }
      })
      .catch((err) => {
        if (!cancelled) {
          setRouteError(err instanceof Error ? err.message : String(err));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [storyEntityUuid]);

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
    if (pageNavigationIsNoop(page, nextPage, recordRoute)) return;
    if (!confirmStoryNavigation(storyDirty)) {
      return;
    }
    if (window.location.hash) {
      window.location.hash = '';
    }
    setStoryDirty(false);
    setRecordRoute(null);
    setStoryEntityUuid(null);
    setStorySubjectIri(null);
    // An explicit page choice outranks the remembered Story origin, and
    // retires any deep-link failure with it.
    pageBeforeStoryRef.current = null;
    setRouteError(null);
    setPage(nextPage);
  };

  // Route through the canonical Story hash so the destination is refreshable
  // and linkable; only a subject with no addressable UUID falls back to
  // in-memory navigation.
  // The displayed Story changed from inside the page (selector, topic, saved
  // recipe). The URL must stop naming the old subject, or a refresh would
  // regenerate something the user is no longer looking at. Replace rather than
  // push, so Back still returns to wherever the Story was launched from.
  const syncStoryHash = (subjectIri: string | null) => {
    const next = (subjectIri && storyEntityHash(subjectIri)) || '';
    if (window.location.hash === next) return;
    window.history.replaceState(
      null,
      '',
      `${window.location.pathname}${window.location.search}${next}`,
    );
    acceptedHashRef.current = next;
    // `replaceState` fires neither `hashchange` nor `popstate`, so the route
    // state the hash handler would have set has to be written here. Without it
    // a Story generated from the hashless page leaves `storyEntityUuid` null,
    // and backing out of an evidence record later looks like a FIRST visit to
    // that Story — tearing down the mounted reader to regenerate what is
    // already on screen. Recording the subject as already resolved is the point.
    const uuid = storyEntityUuidFromHash(next);
    resolvedSubjectUuidRef.current = uuid;
    setStoryEntityUuid(uuid);
    setStorySubjectIri(uuid ? subjectIri : null);
  };

  const navigateStory = (subjectIri: string) => {
    const hash = storyEntityHash(subjectIri);
    if (hash) {
      // Remember where this was launched from: the hash entry is what Back
      // returns to, and a hashless origin (Debt, Chat) has nothing else to
      // restore it from.
      if (page !== 'stories') pageBeforeStoryRef.current = page;
      window.location.hash = hash;
      return;
    }
    if (window.location.hash) {
      window.location.hash = '';
    }
    setRecordRoute(null);
    setStoryEntityUuid(null);
    setStorySubjectIri(subjectIri);
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
      {routeError && (
        <Alert severity="error" sx={{ m: 2 }} onClose={() => setRouteError(null)}>
          {routeError}
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
          {/* Keep Story mounted behind linked records so returning preserves reader/editor state. */}
          <Box sx={{ display: recordRoute?.kind === 'record' ? 'none' : 'block', height: '100%' }}>
            <StoriesPage
              onNavigateRecord={navigateRecord}
              initialSubjectIri={storySubjectIri}
              subjectResolving={Boolean(storyEntityUuid) && !storySubjectIri}
              onSubjectChange={syncStoryHash}
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
