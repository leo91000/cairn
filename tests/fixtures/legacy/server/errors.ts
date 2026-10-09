export class AppError extends Error {
  constructor(
    public statusCode: number,
    message: string,
  ) {
    super(message)
  }
}
export function requireValue<T>(
  value: T | undefined,
  message = 'Not found',
): T {
  if (value === undefined)
    throw new AppError(404, message)
  return value
}
