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
      { keys: ['G', 'M'], label: 'Go to Missions' },
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
      { keys: ['/'], label: 'Filter the Fil' },
    ],
  },
  {
    title: 'Conversation',
    items: [
      { keys: ['R'], label: 'Reply' },
      { keys: ['Enter'], label: 'Send, or queue while the agent works' },
      { keys: ['Shift', 'Enter'], label: 'New line' },
      { keys: ['Alt', 'Enter'], label: 'Steer the running turn' },
      { keys: ['Esc'], label: 'Leave the message field' },
    ],
  },
]
