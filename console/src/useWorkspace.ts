import { useCallback, useEffect, useState } from 'react'

import { api } from './api/client'
import type { WorkspaceDetail } from './api/generated/WorkspaceDetail'

const POLL_MS = 3000

/**
 * The current workspace (the default one, for now) with its channels and
 * members, polled; `refresh` reloads immediately.
 */
export function useWorkspace() {
  const [workspace, setWorkspace] = useState<WorkspaceDetail | null>(null)
  const [generation, setGeneration] = useState(0)
  const refresh = useCallback(() => setGeneration((g) => g + 1), [])

  useEffect(() => {
    let active = true
    const poll = async () => {
      try {
        const [first] = await api.listWorkspaces()
        const detail = first ? await api.getWorkspace(first.id) : null
        if (active) setWorkspace(detail)
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

  return { workspace, refresh }
}
