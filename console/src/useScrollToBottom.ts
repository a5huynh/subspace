import { useEffect, useLayoutEffect, useRef } from 'react'

/** Keep a scroll container pinned to the bottom while the user is near it. */
export function useScrollToBottom(deps: unknown[]) {
  const ref = useRef<HTMLDivElement>(null)
  const pinned = useRef(true)

  useEffect(() => {
    const el = ref.current
    if (!el) return
    const onScroll = () => {
      pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80
    }
    el.addEventListener('scroll', onScroll)
    return () => el.removeEventListener('scroll', onScroll)
  }, [])

  useLayoutEffect(() => {
    const el = ref.current
    if (el && pinned.current) el.scrollTop = el.scrollHeight
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps)

  return ref
}
