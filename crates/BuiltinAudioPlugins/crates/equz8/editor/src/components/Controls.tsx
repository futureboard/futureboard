import type { ReactNode } from 'react'
import { motion } from 'motion/react'

/** Compact On/Off lamp switch — MixStation chrome language. */
export function BypassSwitch({
  on,
  label,
  accent = 'var(--color-signal)',
  disabled = false,
  onToggle,
}: {
  on: boolean
  label: string
  accent?: string
  disabled?: boolean
  onToggle: () => void
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      disabled={disabled}
      onClick={onToggle}
      className="flex h-[22px] shrink-0 cursor-pointer items-center gap-1.5 rounded border px-1.5 text-[10px] font-semibold tracking-wide transition-colors duration-150 disabled:cursor-not-allowed disabled:opacity-40"
      style={{
        borderColor: on ? accent : 'var(--color-hairline-hi)',
        color: on ? accent : 'var(--color-ink-dim)',
        background: on ? `color-mix(in srgb, ${accent} 12%, transparent)` : 'transparent',
      }}
    >
      <motion.span
        layout
        aria-hidden
        className="h-1.5 w-1.5 rounded-full"
        style={{ background: on ? accent : 'var(--color-hairline-hi)' }}
        transition={{ type: 'spring', stiffness: 520, damping: 34 }}
      />
      {on ? 'On' : 'Off'}
    </button>
  )
}

/** Ghost icon button with a 28px hit target. */
export function IconButton({
  label,
  onClick,
  children,
  active = false,
  disabled = false,
  className = '',
}: {
  label: string
  onClick?: () => void
  children: ReactNode
  active?: boolean
  disabled?: boolean
  className?: string
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={onClick}
      className={`grid h-7 w-7 shrink-0 cursor-pointer place-items-center rounded transition-colors duration-150 hover:bg-white/6 hover:text-ink disabled:cursor-not-allowed disabled:opacity-30 ${
        active ? 'text-ink' : 'text-ink-dim'
      } ${className}`}
    >
      {children}
    </button>
  )
}
