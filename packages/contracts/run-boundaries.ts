/** Temporary node-domain payload references for the generated run view (A9/A10). */
export interface RunStorage {
  mode: string
  waitingFor?: string | null
  dirtyBytes?: number
  localBytes?: number
  dirtySince?: number | null
  freeBytes?: number
  reserveBytes?: number
}
