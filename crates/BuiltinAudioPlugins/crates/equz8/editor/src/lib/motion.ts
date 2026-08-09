import { animate, createTimeline, stagger, type JSAnimation } from 'animejs'

const reducedMotion =
  typeof window !== 'undefined' &&
  window.matchMedia('(prefers-reduced-motion: reduce)').matches

/** Soft entrance for the editor shell — Pro-Q calm, Serum presence. */
export function playEditorIntro(root: HTMLElement | null) {
  if (!root || reducedMotion) return null
  const header = root.querySelector('.editor-header')
  const stage = root.querySelector('.stage')
  const rack = root.querySelector('.control-rack')
  const chips = root.querySelectorAll('.band-chip')
  const knobs = root.querySelectorAll('.knob')

  const timeline = createTimeline({ defaults: { ease: 'outExpo' } })
  if (header) {
    timeline.add(header, { opacity: [0, 1], y: [-8, 0], duration: 420 }, 0)
  }
  if (stage) {
    timeline.add(stage, { opacity: [0, 1], scale: [0.985, 1], duration: 560 }, 60)
  }
  if (chips.length) {
    timeline.add(
      chips,
      {
        opacity: [0, 1],
        y: [-6, 0],
        delay: stagger(28),
        duration: 360,
      },
      180,
    )
  }
  if (rack) {
    timeline.add(rack, { opacity: [0, 1], y: [14, 0], duration: 480 }, 140)
  }
  if (knobs.length) {
    timeline.add(
      knobs,
      {
        opacity: [0, 1],
        scale: [0.92, 1],
        delay: stagger(32),
        duration: 380,
      },
      220,
    )
  }
  return timeline
}

/** Pulse a selected band node the way Pro-Q flashes the active handle. */
export function pulseBandNode(node: Element | null): JSAnimation | null {
  if (!node || reducedMotion) return null
  return animate(node.querySelectorAll('.node-halo, .node-ring'), {
    scale: [1, 1.18, 1],
    duration: 420,
    ease: 'outElastic(1, 0.55)',
  })
}

/** Brief rack flash when switching bands or loading a preset. */
export function flashControlRack(rack: HTMLElement | null) {
  if (!rack || reducedMotion) return null
  return animate(rack, {
    filter: ['brightness(1.18)', 'brightness(1)'],
    duration: 320,
    ease: 'outQuad',
  })
}

/** Knob pointer snap when value jumps (preset / double-click reset). */
export function snapKnobDial(dial: HTMLElement | null) {
  if (!dial || reducedMotion) return null
  return animate(dial, {
    scale: [1, 1.06, 1],
    duration: 280,
    ease: 'outBack(1.6)',
  })
}

/** Power / bypass: dim the stage without hiding structure. */
export function tweenBypass(stage: HTMLElement | null, bypassed: boolean) {
  if (!stage || reducedMotion) {
    if (stage) stage.style.opacity = bypassed ? '0.55' : '1'
    return null
  }
  return animate(stage, {
    opacity: bypassed ? 0.55 : 1,
    duration: 280,
    ease: 'inOutQuad',
  })
}
