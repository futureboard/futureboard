import { useEffect, useRef } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { requestSelectInstance, type BoundInstance } from './bridge'

export type InstanceRouteSyncState = {
  approvedInstanceId: string | null
  ignoredRouteId: string | null
  requestedRouteId: string | null
}

export function createInstanceRouteSyncState(): InstanceRouteSyncState {
  return {
    approvedInstanceId: null,
    ignoredRouteId: null,
    requestedRouteId: null,
  }
}

/**
 * Record one authoritative native selection and return the route it should
 * mirror to. The route visible in this render is ignored once: both effects run
 * from the same render, so without that guard it would immediately request the
 * previously selected instance back from native.
 */
export function acceptNativeSelection(
  sync: InstanceRouteSyncState,
  instanceId: string | null,
  routeInstanceId: string | null,
): string | null {
  sync.approvedInstanceId = instanceId
  sync.requestedRouteId = null
  sync.ignoredRouteId =
    instanceId && routeInstanceId && routeInstanceId !== instanceId ? routeInstanceId : null
  return instanceId && routeInstanceId !== instanceId ? instanceId : null
}

/** Return an unapproved route for native validation, at most once. */
export function selectionRequestForRoute(
  sync: InstanceRouteSyncState,
  routeInstanceId: string | null,
): string | null {
  if (!routeInstanceId || routeInstanceId === sync.approvedInstanceId) {
    sync.ignoredRouteId = null
    sync.requestedRouteId = null
    return null
  }
  if (routeInstanceId === sync.ignoredRouteId) {
    sync.ignoredRouteId = null
    return null
  }
  if (routeInstanceId === sync.requestedRouteId) return null
  sync.requestedRouteId = routeInstanceId
  return routeInstanceId
}

/**
 * Keeps the URL and the native instance binding in step.
 *
 * The route **mirrors** the binding; it never drives it. Native owns which
 * insert this page is attached to — the `bindingGeneration` and `stateRevision`
 * guards in `bridge.ts` exist so a stale or cross-instance message can never
 * write parameters to the wrong plug-in, and a route that could bind on its own
 * would defeat them.
 *
 * So the two directions are deliberately asymmetric:
 *
 * - binding → route: an approved `selectInstance` rewrites the URL, with
 *   `replace` so rebinding does not pile up history entries.
 * - route → binding: a hash edit or a back/forward posts
 *   `requestSelectInstance` and then waits. Native validates it and answers
 *   with a real `selectInstance`, or ignores it and the page stays where it is.
 *
 * This matches the contract rodharerist's editor already uses.
 */
export function InstanceRoute({ instance }: { instance: BoundInstance | null }) {
  const navigate = useNavigate()
  const { instanceId: routeInstanceId } = useParams<{ instanceId: string }>()
  const sync = useRef<InstanceRouteSyncState>(createInstanceRouteSyncState())
  const routeRef = useRef<string | null>(routeInstanceId ?? null)
  routeRef.current = routeInstanceId ?? null

  // Binding → route. `instance` is a fresh object for every selectInstance
  // message, including native restoring the current binding after rejecting a
  // route request; do not make this effect depend on the route itself.
  useEffect(() => {
    const target = acceptNativeSelection(
      sync.current,
      instance?.instanceId ?? null,
      routeRef.current,
    )
    if (target) {
      void navigate(`/instance/${encodeURIComponent(target)}`, { replace: true })
    }
  }, [instance, navigate])

  // Route → binding, via native approval only.
  useEffect(() => {
    const target = selectionRequestForRoute(sync.current, routeInstanceId ?? null)
    if (target) requestSelectInstance(target)
  }, [routeInstanceId])

  return null
}
