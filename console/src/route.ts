import { useEffect, useState } from 'react'

export type Route =
  | { page: 'home' }
  | { page: 'new' }
  | { page: 'worker'; id: string }
  | { page: 'settings'; id: string }
  | { page: 'new-workspace' }
  | { page: 'workspace'; id: string }
  | { page: 'workspace-settings'; id: string }

export function parseRoute(hash: string): Route {
  const parts = hash.replace(/^#\/?/, '').split('/').filter(Boolean).map(decodeURIComponent)
  if (parts[0] === 'new') return { page: 'new' }
  if (parts[0] === 'workspaces' && parts[1] === 'new') return { page: 'new-workspace' }
  if (parts[0] === 'workspaces' && parts[1]) {
    return parts[2] === 'settings'
      ? { page: 'workspace-settings', id: parts[1] }
      : { page: 'workspace', id: parts[1] }
  }
  if (parts[0] === 'workers' && parts[1]) {
    return parts[2] === 'settings' ? { page: 'settings', id: parts[1] } : { page: 'worker', id: parts[1] }
  }
  return { page: 'home' }
}

export function href(route: Route): string {
  switch (route.page) {
    case 'home':
      return '#/'
    case 'new':
      return '#/new'
    case 'worker':
      return `#/workers/${encodeURIComponent(route.id)}`
    case 'settings':
      return `#/workers/${encodeURIComponent(route.id)}/settings`
    case 'new-workspace':
      return '#/workspaces/new'
    case 'workspace':
      return `#/workspaces/${encodeURIComponent(route.id)}`
    case 'workspace-settings':
      return `#/workspaces/${encodeURIComponent(route.id)}/settings`
  }
}

export function navigate(route: Route) {
  location.hash = href(route)
}

/** The current hash route, updated on navigation. */
export function useRoute(): Route {
  const [route, setRoute] = useState(() => parseRoute(location.hash))
  useEffect(() => {
    const onChange = () => setRoute(parseRoute(location.hash))
    window.addEventListener('hashchange', onChange)
    return () => window.removeEventListener('hashchange', onChange)
  }, [])
  return route
}
