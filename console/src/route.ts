import { useEffect, useState } from 'react'

export type Route =
  | { page: 'home' }
  | { page: 'new' }
  | { page: 'worker'; id: string }
  | { page: 'settings'; id: string }
  | { page: 'new-channel' }
  | { page: 'channel'; id: string }
  | { page: 'channel-settings'; id: string }

export function parseRoute(hash: string): Route {
  const parts = hash.replace(/^#\/?/, '').split('/').filter(Boolean).map(decodeURIComponent)
  if (parts[0] === 'new') return { page: 'new' }
  if (parts[0] === 'channels' && parts[1] === 'new') return { page: 'new-channel' }
  if (parts[0] === 'channels' && parts[1]) {
    return parts[2] === 'settings'
      ? { page: 'channel-settings', id: parts[1] }
      : { page: 'channel', id: parts[1] }
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
    case 'new-channel':
      return '#/channels/new'
    case 'channel':
      return `#/channels/${encodeURIComponent(route.id)}`
    case 'channel-settings':
      return `#/channels/${encodeURIComponent(route.id)}/settings`
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
