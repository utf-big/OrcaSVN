/**
 * Waits until a checkout has turned its target into a working copy.
 *
 * Adding the directory to the workspace before `.svn` exists makes every
 * SVN-backed view query a directory that is not a working copy yet, and SVN
 * answers `E155007`. SVN creates `.svn` before it transfers any file, so the wait
 * is normally under a second and the workspace still appears "immediately".
 */
export interface WaitForWorkingCopyOptions {
  attempts?: number
  intervalMs?: number
  /** Checked between probes so a cancelled checkout stops waiting. */
  shouldStop?: () => boolean
  sleep?: (durationMs: number) => Promise<void>
}

const defaultSleep = (durationMs: number) => new Promise<void>(resolve => {
  setTimeout(resolve, durationMs)
})

export async function waitForWorkingCopy(
  probe: () => Promise<boolean>,
  options: WaitForWorkingCopyOptions = {},
): Promise<boolean> {
  const attempts = options.attempts ?? 60
  const intervalMs = options.intervalMs ?? 250
  const shouldStop = options.shouldStop ?? (() => false)
  const sleep = options.sleep ?? defaultSleep

  for (let attempt = 0; attempt < attempts; attempt += 1) {
    if (shouldStop()) return false
    try {
      if (await probe()) return true
    } catch {
      // The directory may be in flux while SVN creates it; keep probing.
    }
    if (shouldStop()) return false
    if (attempt + 1 < attempts) await sleep(intervalMs)
  }
  return false
}
