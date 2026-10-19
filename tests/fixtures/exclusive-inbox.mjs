import fs from 'node:fs'

// Guest inbox imports extract an archive over the previous snapshot. Like the
// real adapters, poll again while the file is absent or its JSON is incomplete.
export function finishRequested(inbox) {
  let messages
  try {
    messages = JSON.parse(fs.readFileSync(inbox, 'utf8'))
  }
  catch (error) {
    if (error.code === 'ENOENT' || error instanceof SyntaxError)
      return false
    throw error
  }

  return Array.isArray(messages) && messages.some(message => message?.text === 'finish')
}
