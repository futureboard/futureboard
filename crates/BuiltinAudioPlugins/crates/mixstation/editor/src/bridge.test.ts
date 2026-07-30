import { describe, expect, test } from 'bun:test'
import {
  BRIDGE_PROTOCOL_VERSION,
  PLUGIN_ID,
  defaults,
  makeSetParamsMessage,
  parseParams,
} from './bridge'

describe('MixStation bridge contract', () => {
  test('parses direct and wrapped authoritative state', () => {
    expect(parseParams(defaults)).toEqual(defaults)
    expect(parseParams({ params: defaults })).toEqual(defaults)
  })

  test('rejects incomplete or non-finite state', () => {
    expect(parseParams({ ...defaults, widthPct: Number.NaN })).toBeNull()
    const { limiterReleaseMs: _, ...incomplete } = defaults
    expect(parseParams(incomplete)).toBeNull()
  })

  test('builds the shared batched setParams envelope', () => {
    expect(
      makeSetParamsMessage(
        {
          pluginId: PLUGIN_ID,
          instanceId: 'instance-7',
          bindingGeneration: 3,
        },
        [{ id: 'widthPct', value: 112 }],
      ),
    ).toEqual({
      type: 'futureboard.setParams',
      protocolVersion: BRIDGE_PROTOCOL_VERSION,
      pluginId: PLUGIN_ID,
      instanceId: 'instance-7',
      bindingGeneration: 3,
      params: [{ id: 'widthPct', value: 112 }],
    })
  })
})
