import { useEffect, useState } from 'react'

import { api } from './api/client'
import type { Health } from './api/generated/Health'

// Placeholder until the console UI lands: confirms the dev proxy reaches the server.
export default function App() {
  const [health, setHealth] = useState<Health | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    api.health().then(setHealth, (e: unknown) => setError(String(e)))
  }, [])

  return (
    <main>
      <h1>Subspace</h1>
      {health && <p>Server ok (epoch {health.epoch})</p>}
      {error && <p>Can't reach subspace-server: {error}</p>}
    </main>
  )
}
