// Shared interactive styles. Call sites own layout; these variants own controls.
export const buttonBase = 'button inline-flex shrink-0 items-center justify-center gap-[9px] rounded-lg border font-semibold whitespace-nowrap transition-[background,transform,box-shadow] duration-150 disabled:cursor-not-allowed disabled:opacity-50 disabled:shadow-none disabled:transform-none focus-visible:outline-2 focus-visible:outline-offset-3 focus-visible:outline-accent phone:min-h-11'
export const buttonVariants = {
  'default': 'border-line bg-transparent text-ink enabled:hover:bg-hover',
  'primary': 'primary border-transparent bg-brand text-white enabled:hover:bg-brand-hover enabled:active:scale-[.98]',
  'danger': 'danger border-danger bg-danger text-white enabled:hover:brightness-95',
  'danger-outline': 'danger-outline border-control bg-raised text-danger enabled:hover:bg-danger-surface',
} as const
export const buttonSizes = {
  default: 'px-4 py-[11px] text-sm',
  small: 'small px-3 py-2 text-2xs',
} as const
export type ButtonVariant = keyof typeof buttonVariants
export type ButtonSize = keyof typeof buttonSizes
export const iconButton = 'icon-button inline-flex size-8 shrink-0 items-center justify-center rounded-lg text-muted transition-colors hover:bg-hover hover:text-ink focus-visible:outline-2 focus-visible:outline-offset-3 focus-visible:outline-accent disabled:cursor-not-allowed disabled:opacity-50 phone:min-h-11 phone:min-w-11'
