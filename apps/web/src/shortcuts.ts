/** Single-key shortcuts never fire while the user types in a field. */
export function typingTarget(target: EventTarget | null) {
  return target instanceof HTMLElement && (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName) || !!target.closest('[role="combobox"], [role="listbox"]'))
}

export const modifier = /Mac|iPhone|iPad/.test(navigator.platform) ? '⌘' : 'Ctrl'

export const shortcutGroups: Array<{ title: string, items: Array<{ keys: string[], label: string }> }> = [
  {
    title: 'Anywhere',
    items: [
      { keys: [modifier, 'K'], label: 'Search and run commands' },
      { keys: ['C'], label: 'New conversation' },
      { keys: ['G', 'F'], label: 'Go to the Fil' },
      { keys: ['G', 'R'], label: 'Go to Rules' },
      { keys: ['G', 'A'], label: 'Go to the Atelier' },
      { keys: ['?'], label: 'Show these shortcuts' },
    ],
  },
  {
    title: 'Fil',
    items: [
      { keys: ['J'], label: 'Next item' },
      { keys: ['K'], label: 'Previous item' },
      { keys: ['Enter'], label: 'Open the selected item' },
      { keys: ['E'], label: 'Mark the selected finished work as read' },
      { keys: ['/'], label: 'Filter the Fil' },
    ],
  },
  {
    title: 'Conversation',
    items: [
      { keys: ['R'], label: 'Reply' },
      { keys: ['Enter'], label: 'Send, steering while the agent works' },
      { keys: ['Shift', 'Enter'], label: 'New line' },
      { keys: ['Alt', 'Enter'], label: 'Send after this turn' },
      { keys: ['Esc'], label: 'Leave the message field' },
    ],
  },
]
