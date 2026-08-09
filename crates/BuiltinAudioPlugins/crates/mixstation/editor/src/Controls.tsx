import type { ReactNode } from 'react'
import { motion } from 'motion/react'

/**
 * Bypass switch. Compact rather than a "giant pill" — the design contract keeps
 * chrome quiet — with the lamp carrying state so it does not read by colour
 * alone.
 */
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

/** Ghost icon button with a 28px hit target around a small glyph. */
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

/** Small labelled toggle used for module-level options. */
export function CheckOption({
  checked,
  label,
  accent,
  disabled = false,
  onChange,
}: {
  checked: boolean
  label: string
  accent: string
  disabled?: boolean
  onChange: (value: boolean) => void
}) {
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className="flex cursor-pointer items-center gap-1.5 rounded px-0.5 py-1 disabled:cursor-not-allowed disabled:opacity-40"
    >
      <span
        aria-hidden
        className="h-3 w-3 shrink-0 rounded-[3px] border transition-colors duration-150"
        style={{
          borderColor: checked ? accent : 'var(--color-hairline-hi)',
          background: checked ? accent : 'transparent',
        }}
      />
      <span className="label-cap">{label}</span>
    </button>
  )
}
