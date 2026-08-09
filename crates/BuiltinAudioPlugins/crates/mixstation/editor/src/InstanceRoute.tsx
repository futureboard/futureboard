import { useEffect, useRef } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { requestSelectInstance, type BoundInstance } from './bridge'

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
  /** The last id we asked native about, so a rejected request is not retried. */
  const requested = useRef<string | null>(null)

  const boundId = instance?.instanceId ?? null

  // Binding → route.
  useEffect(() => {
    if (!boundId || boundId === routeInstanceId) return
    requested.current = boundId
    void navigate(`/instance/${encodeURIComponent(boundId)}`, { replace: true })
  }, [boundId, routeInstanceId, navigate])

  // Route → binding, via native approval only.
  useEffect(() => {
    if (!routeInstanceId || routeInstanceId === boundId) return
    if (requested.current === routeInstanceId) return
    requested.current = routeInstanceId
    requestSelectInstance(routeInstanceId)
  }, [routeInstanceId, boundId])

  return null
}
