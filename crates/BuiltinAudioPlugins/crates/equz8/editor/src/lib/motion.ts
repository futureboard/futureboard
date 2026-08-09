import { animate, createTimeline, stagger, type JSAnimation } from 'animejs'

const reducedMotion =
  typeof window !== 'undefined' &&
  window.matchMedia('(prefers-reduced-motion: reduce)').matches

/** Soft entrance for the editor shell. */
export function playEditorIntro(root: HTMLElement | null) {
  if (!root || reducedMotion) return null
  const header = root.querySelector('header')
  const stage = root.querySelector('.stage')
  const rack = root.querySelector('.control-rack')
  const chips = root.querySelectorAll('.band-chips button')
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

export function pulseBandNode(node: Element | null): JSAnimation | null {
  if (!node || reducedMotion) return null
  return animate(node.querySelectorAll('.node-halo, .node-ring'), {
    scale: [1, 1.18, 1],
    duration: 420,
    ease: 'outElastic(1, 0.55)',
  })
}

export function flashControlRack(rack: HTMLElement | null) {
  if (!rack || reducedMotion) return null
  return animate(rack, {
    filter: ['brightness(1.18)', 'brightness(1)'],
    duration: 320,
    ease: 'outQuad',
  })
}

export function snapKnobDial(dial: HTMLElement | null) {
  if (!dial || reducedMotion) return null
  return animate(dial, {
    scale: [1, 1.06, 1],
    duration: 280,
    ease: 'outBack(1.6)',
  })
}

/** Brief brightness flash; steady bypass look is CSS `.is-bypassed`. */
export function tweenBypass(stage: HTMLElement | null, _bypassed: boolean) {
  if (!stage || reducedMotion) return null
  return animate(stage, {
    filter: ['brightness(1.12)', 'brightness(1)'],
    duration: 280,
    ease: 'outQuad',
  })
}
