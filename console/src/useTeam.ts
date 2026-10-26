import { useCallback, useEffect, useState } from 'react'

import { api } from './api/client'
import type { TeamDetail } from './api/generated/TeamDetail'

const POLL_MS = 3000

/**
 * The current team (the default one, for now) with its workspaces and
 * members, polled; `refresh` reloads immediately.
 */
export function useTeam() {
  const [team, setTeam] = useState<TeamDetail | null>(null)
  const [generation, setGeneration] = useState(0)
  const refresh = useCallback(() => setGeneration((g) => g + 1), [])

  useEffect(() => {
    let active = true
    const poll = async () => {
      try {
        const [first] = await api.listTeams()
        const detail = first ? await api.getTeam(first.id) : null
        if (active) setTeam(detail)
      } catch {
        // The worker list reports connection problems; keep the last value.
      }
    }
    void poll()
    const timer = setInterval(() => void poll(), POLL_MS)
    return () => {
      active = false
      clearInterval(timer)
    }
  }, [generation])

  return { team, refresh }
}
