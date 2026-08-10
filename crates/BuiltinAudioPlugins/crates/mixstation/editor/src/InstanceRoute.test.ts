import { describe, expect, test } from 'bun:test'
import {
  acceptNativeSelection,
  createInstanceRouteSyncState,
  selectionRequestForRoute,
} from './InstanceRoute'

describe('MixStation instance route synchronization', () => {
  test('does not request the previous route after a native channel switch', () => {
    const sync = createInstanceRouteSyncState()

    expect(acceptNativeSelection(sync, 'track-a::insert-1', null)).toBe(
      'track-a::insert-1',
    )
    expect(selectionRequestForRoute(sync, 'track-a::insert-1')).toBeNull()

    expect(
      acceptNativeSelection(sync, 'track-b::insert-2', 'track-a::insert-1'),
    ).toBe('track-b::insert-2')
    expect(selectionRequestForRoute(sync, 'track-a::insert-1')).toBeNull()
    expect(selectionRequestForRoute(sync, 'track-b::insert-2')).toBeNull()
  })

  test('asks native once when the route changes independently', () => {
    const sync = createInstanceRouteSyncState()
    acceptNativeSelection(sync, 'track-a::insert-1', 'track-a::insert-1')

    expect(selectionRequestForRoute(sync, 'track-b::insert-2')).toBe(
      'track-b::insert-2',
    )
    expect(selectionRequestForRoute(sync, 'track-b::insert-2')).toBeNull()
  })

  test('restores the approved route when native rejects a request', () => {
    const sync = createInstanceRouteSyncState()
    acceptNativeSelection(sync, 'track-a::insert-1', 'track-a::insert-1')
    expect(selectionRequestForRoute(sync, 'unknown')).toBe('unknown')

    expect(acceptNativeSelection(sync, 'track-a::insert-1', 'unknown')).toBe(
      'track-a::insert-1',
    )
    expect(selectionRequestForRoute(sync, 'unknown')).toBeNull()
  })
})
