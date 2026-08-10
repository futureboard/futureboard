import { useEffect, useRef, useState, type ReactNode } from 'react'

/** Shared control chrome, so every header element sits on the same rhythm. */
export const CONTROL =
  'glass-pill flex h-8 items-center gap-2 rounded-full px-3 text-[11px] text-white/80'

export const LABEL = 'text-[9px] font-medium uppercase tracking-[0.14em] text-white/35'

export function Chevron({ open }: { open: boolean }) {
  return (
    <svg
      width={9}
      height={9}
      viewBox="0 0 10 10"
      className={`shrink-0 text-white/35 transition-transform duration-150 ${open ? 'rotate-180' : ''}`}
    >
      <path d="M1.5 3.5 5 7 8.5 3.5" fill="none" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  )
}

interface MenuProps {
  trigger: (open: boolean) => ReactNode
  children: (close: () => void) => ReactNode
  align?: 'start' | 'end'
  triggerClass?: string
  panelClass?: string
  title?: string
}

/** Popover anchored under its trigger. Closes on outside click or Escape. */
export function Menu({
  trigger,
  children,
  align = 'start',
  triggerClass = CONTROL,
  panelClass = '',
  title,
}: MenuProps) {
  const [open, setOpen] = useState(false)
  const wrap = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const onDown = (ev: PointerEvent) => {
      if (!wrap.current?.contains(ev.target as Node)) setOpen(false)
    }
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === 'Escape') setOpen(false)
    }
    window.addEventListener('pointerdown', onDown)
    window.addEventListener('keydown', onKey)
    return () => {
      window.removeEventListener('pointerdown', onDown)
      window.removeEventListener('keydown', onKey)
    }
  }, [open])

  return (
    <div ref={wrap} className="relative">
      <button
        type="button"
        title={title}
        onClick={() => setOpen((o) => !o)}
        className={`${triggerClass} ${open ? 'glass-pill-on' : ''}`}
      >
        {trigger(open)}
      </button>
      {open && (
        <div
          // A darker base than the bar itself: a popover has to stay legible over
          // whatever the spectrum is doing behind it.
          className={`glass absolute top-full z-50 mt-2 overflow-hidden rounded-2xl bg-[#131315]/78 p-1.5 ${
            align === 'end' ? 'right-0' : 'left-0'
          } ${panelClass}`}
        >
          {children(() => setOpen(false))}
        </div>
      )}
    </div>
  )
}

export function MenuItem({
  children,
  onClick,
  selected = false,
  danger = false,
}: {
  children: ReactNode
  onClick: () => void
  selected?: boolean
  danger?: boolean
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex w-full items-center justify-between gap-4 rounded-xl px-2.5 py-1.5 text-left text-[11px] transition ${
        danger
          ? 'text-white/60 hover:bg-red-500/15 hover:text-red-300'
          : selected
            ? 'bg-neon/18 text-mochi'
            : 'text-white/70 hover:bg-white/10 hover:text-white'
      }`}
    >
      <span className="truncate">{children}</span>
      {selected && (
        <svg width={11} height={11} viewBox="0 0 12 12" className="shrink-0">
          <path d="M2 6.5 4.7 9 10 3.5" fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      )}
    </button>
  )
}

export function MenuDivider() {
  return <div className="my-1 h-px bg-white/8" />
}

export function MenuLabel({ children }: { children: ReactNode }) {
  return <div className={`${LABEL} px-2.5 pb-1 pt-1.5`}>{children}</div>
}

interface DropdownProps {
  label: string
  value: string
  options: { value: string; label: string }[]
  onChange: (v: string) => void
  align?: 'start' | 'end'
}

/** Compact "LABEL value ⌄" picker — replaces a row of segmented buttons. */
export function Dropdown({ label, value, options, onChange, align = 'start' }: DropdownProps) {
  const current = options.find((o) => o.value === value)
  return (
    <Menu
      align={align}
      panelClass="min-w-[132px]"
      trigger={(open) => (
        <>
          <span className={LABEL}>{label}</span>
          <span className="font-medium text-white/90">{current?.label ?? '—'}</span>
          <Chevron open={open} />
        </>
      )}
    >
      {(close) =>
        options.map((o) => (
          <MenuItem
            key={o.value}
            selected={o.value === value}
            onClick={() => {
              onChange(o.value)
              close()
            }}
          >
            {o.label}
          </MenuItem>
        ))
      }
    </Menu>
  )
}
