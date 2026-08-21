/**
 * Hash routing for the workbench: parsing a location into a route, deriving a
 * link from an IRI, and the unsaved-Story navigation guard.
 *
 * Separated from the shell because none of it renders anything — it is pure,
 * and it is where the workbench's addressing rules live, so it is worth reading
 * and testing on its own.
 */
import { PageKey } from './components/layout/AppShell';
import {
  artifactTargetForIri,
  ArtifactKind,
  ArtifactTarget,
} from './components/artifacts/LinkedMarkdown';

export function confirmStoryNavigation(
  hasUnsavedChanges: boolean,
  confirmDiscard: () => boolean = () => window.confirm('Discard your unsaved Story changes?'),
): boolean {
  return !hasUnsavedChanges || confirmDiscard();
}

export function confirmStoryHashNavigation(
  hasUnsavedChanges: boolean,
  // Any recognized destination — a record route or a Story deep link. Only its
  // presence matters: leaving the editor for one needs the same confirmation.
  nextRoute: unknown,
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

/**
 * The canonical, refreshable Story deep link: `#/stories/entity/{uuid}`.
 *
 * It carries a record UUID rather than an IRI so it stays consistent with
 * every other workbench route, and so the daemon — not the browser — decides
 * which entity that UUID names.
 */
/**
 * Whether a Story deep link's subject still has to be looked up.
 *
 * Following an evidence citation and pressing Back returns to the SAME Story
 * hash, so the resolution effect re-enters with a uuid it has already resolved.
 * Resolving again would clear the subject, flip `subjectResolving`, and destroy
 * the Story that is deliberately kept mounted behind the record overlay to
 * preserve reader progress and graded answers. A DIFFERENT uuid must still
 * resolve, even though a stale subject is on screen.
 */
export function storySubjectNeedsResolving(
  uuid: string | null,
  resolvedUuid: string | null,
  subjectIri: string | null,
): uuid is string {
  if (!uuid) return false;
  return !(resolvedUuid === uuid && Boolean(subjectIri));
}

export function storyEntityUuidFromHash(hash: string): string | null {
  const match = /^#\/stories\/entity\/([^/]+)$/.exec(hash);
  if (!match) {
    return null;
  }
  try {
    const uuid = decodeURIComponent(match[1]);
    return uuid && !uuid.includes('/') ? uuid : null;
  } catch {
    return null;
  }
}

/**
 * A refreshable Story hash, or null when the IRI is not addressable by the
 * record route.
 *
 * The daemon resolves a UUID by matching subjects that END WITH `/{uuid}`, so
 * only a slash-delimited final segment round-trips. A fragment-addressed IRI
 * like `https://example.test/records#decision` would otherwise advertise
 * `#/stories/entity/decision`, which resolves to nothing — or to an unrelated
 * subject that happens to end in `/decision`.
 */
export function storyEntityHash(iri: string): string | null {
  const uuid = uuidFromIri(iri);
  return uuid ? `#/stories/entity/${encodeURIComponent(uuid)}` : null;
}

/**
 * An IRI's final segment, but only when a workbench route can resolve it. The
 * daemon matches subjects ending in `/{uuid}`, so a fragment-addressed IRI like
 * `https://example.test/records#decision` names nothing it can find — or an
 * unrelated record ending in `/decision`. Guarding here rather than at each
 * call site is what keeps every route helper honest, not just the newest one.
 */
function uuidFromIri(iri: string): string | null {
  const slash = iri.lastIndexOf('/');
  if (slash < 0 || iri.lastIndexOf('#') > slash) {
    return null;
  }
  return iri.slice(slash + 1) || null;
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

export function routeForArtifact(target: ArtifactTarget): ArtifactRoute | null {
  const uuid = uuidFromIri(target.iri);
  return uuid ? { kind: target.kind, uuid } : null;
}

export function hashForRoute(route: RecordRoute): string {
  return `#/${route.kind}/${encodeURIComponent(route.uuid)}`;
}
