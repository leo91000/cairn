interface MessageTime {
  datetime: string
  title: string
  label: string
}

/** One reader's timestamps, retained only while its messages remain in the rendered history. */
export class MessageTimes {
  private context = ''
  private previous = new Map<number, MessageTime>()
  private current = new Map<number, MessageTime>()

  beginRender() {
    // Check once per parent render, just as the old Date calls picked up the current
    // browser locale and zone on every render. Include calendar, numbering and hour cycle.
    const context = JSON.stringify(new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit' }).resolvedOptions())
    this.previous = context === this.context ? this.current : new Map()
    this.current = new Map()
    this.context = context
  }

  format(timestamp: number): MessageTime {
    let result = this.current.get(timestamp) ?? this.previous.get(timestamp)
    if (!result) {
      const date = new Date(timestamp)
      result = {
        datetime: date.toISOString(),
        title: date.toLocaleString(),
        label: date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }),
      }
    }

    this.current.set(timestamp, result)
    return result
  }
}
