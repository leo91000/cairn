import type { ObjectDirective } from 'vue'

/** Move sanitized document nodes directly; retain equal prefix/suffix blocks in the reader. */
function updateHtml(element: HTMLElement, next: HTMLElement) {
  const previous = [...element.childNodes]
  const incoming = [...next.childNodes]
  let start = 0
  while (start < previous.length && start < incoming.length && previous[start]!.isEqualNode(incoming[start]!))
    start++

  let oldEnd = previous.length
  let newEnd = incoming.length
  while (oldEnd > start && newEnd > start && previous[oldEnd - 1]!.isEqualNode(incoming[newEnd - 1]!)) {
    oldEnd--
    newEnd--
  }

  const anchor = previous[oldEnd] ?? null
  for (let index = start; index < oldEnd; index++)
    previous[index]!.remove()

  const fragment = document.createDocumentFragment()
  for (let index = start; index < newEnd; index++)
    fragment.append(incoming[index]!)
  element.insertBefore(fragment, anchor)
  // The computed container must not retain the unused copies of unchanged blocks.
  next.replaceChildren()
}

// Only Markdown.vue supplies this directive: its value has passed through DOMPurify,
// highlighting, table wrapping and the existing link/image transformations.
export const markdownHtml: ObjectDirective<HTMLElement, HTMLElement> = {
  beforeMount(element, { value }) {
    updateHtml(element, value)
  },
  updated(element, { value, oldValue }) {
    if (value !== oldValue)
      updateHtml(element, value)
  },
}
