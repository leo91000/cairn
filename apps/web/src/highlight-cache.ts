import { highlight } from './highlight'

// Bounds include both the cache key and generated HTML, in UTF-16 code units.
const MAX_CHARACTERS = 256_000
const MAX_ENTRIES = 64

/** Owned by one Markdown component; unchanged code need not be highlighted per fragment. */
export class HighlightCache {
  private entries = new Map<string, string>()
  private previous = new Map<string, string>()
  private characters = 0

  beginRender() {
    this.previous = this.entries
    this.entries = new Map()
    this.characters = 0
  }

  render(code: string, language = 'plaintext') {
    const key = `${language.length}:${language}${code}`
    const cached = this.entries.get(key)
    if (cached !== undefined)
      return cached

    const html = this.previous.get(key) ?? highlight(code, language)
    const characters = key.length + html.length
    // Admit a bounded prefix of each document. Evicting on every miss would make
    // a large sequential scan evict every reusable entry before reaching it again.
    if (this.entries.size >= MAX_ENTRIES || this.characters + characters > MAX_CHARACTERS)
      return html

    this.entries.set(key, html)
    this.characters += characters
    return html
  }
}
