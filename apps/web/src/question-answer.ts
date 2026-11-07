import { z } from 'zod'

const summaryFields = z.array(z.object({
  question: z.string(),
  selected: z.array(z.object({ label: z.string() })),
  comment: z.string(),
})).min(1).max(20)

/** Present Cairn's structured reply in the conversation without changing agent input. */
export function questionAnswerText(text: string, questionId?: unknown): string {
  const prefix = 'My answers to your questions:\n\n'
  if (typeof questionId !== 'string' || !questionId || !text.startsWith(prefix))
    return text

  try {
    const parsed = summaryFields.safeParse(JSON.parse(text.slice(prefix.length)))
    if (!parsed.success)
      return text

    const summaries: string[] = []
    for (const field of parsed.data) {
      const lines = [field.question]
      if (field.selected.length)
        lines.push(`Choices: ${field.selected.map(option => option.label).join(', ')}`)
      if (field.comment)
        lines.push(`${field.selected.length ? 'Comment' : 'Answer'}: ${field.comment}`)
      summaries.push(lines.join('\n'))
    }

    return `Your answers\n\n${summaries.join('\n\n')}`
  }
  catch {
    return text
  }
}
