/** Matches the backend working-set rule for component lifecycle status. */
export function isWorkingSetStatus(status?: string | null): boolean {
  if (status == null) return true;
  return !['proposed', 'rejected', 'superseded', 'deprecated'].includes(status.toLowerCase());
}
