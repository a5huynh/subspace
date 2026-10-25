import { useCallback, useEffect, useState } from 'react'

import { api } from './api/client'
import type { WorkerSummary } from './api/generated/WorkerSummary'

const POLL_MS = 3000

/** The worker list, polled for status changes; `refresh` reloads immediately. */
export function useWorkers() {
  const [workers, setWorkers] = useState<WorkerSummary[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  // Bumping this restarts the effect, which fetches right away.
  const [generation, setGeneration] = useState(0)
  const refresh = useCallback(() => setGeneration((g) => g + 1), [])

  useEffect(() => {
    let active = true
    const poll = () =>
      api.listWorkers().then(
        (list) => {
          if (!active) return
          setWorkers(list)
          setError(null)
        },
        (e: unknown) => active && setError(e instanceof Error ? e.message : String(e)),
      )
    void poll()
    const timer = setInterval(() => void poll(), POLL_MS)
    return () => {
      active = false
      clearInterval(timer)
    }
  }, [generation])

  return { workers, error, refresh }
}
