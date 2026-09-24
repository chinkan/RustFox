import type { StreamFrame } from './types'

/**
 * Minimal SSE frame parser for `fetch` + ReadableStream responses.
 *
 * Spec subset that the Axum `Sse` writer actually emits:
 *   event: <name>\n
 *   data: <payload>\n
 *   \n                        ← dispatch
 *   : comment                 ← ignored (keep-alives may also arrive as
 *                                `event: ping\ndata: ping`)
 *
 * Pure function so it is unit-testable without a network.
 */
export class SseDecoder {
  private buf = ''

  /** Feed a decoded text chunk; returns any complete frames. */
  push(chunk: string): StreamFrame[] {
    this.buf += chunk
    const frames: StreamFrame[] = []
    // A blank line terminates a frame. Normalise CRLF first.
    this.buf = this.buf.replace(/\r\n/g, '\n').replace(/\r/g, '\n')
    let idx: number
    while ((idx = this.buf.indexOf('\n\n')) !== -1) {
      const raw = this.buf.slice(0, idx)
      this.buf = this.buf.slice(idx + 2)
      const frame = parseBlock(raw)
      if (frame) frames.push(frame)
    }
    return frames
  }

  /** Flush any trailing frame that arrived without the final blank line. */
  end(): StreamFrame[] {
    const rest = this.buf.trim()
    this.buf = ''
    if (!rest) return []
    const frame = parseBlock(rest)
    return frame ? [frame] : []
  }
}

function parseBlock(block: string): StreamFrame | null {
  let event = 'message'
  const dataLines: string[] = []
  for (const line of block.split('\n')) {
    if (line.startsWith(':')) continue // comment
    if (line.startsWith('event:')) {
      event = line.slice(6).trim()
    } else if (line.startsWith('data:')) {
      dataLines.push(line.slice(5).replace(/^ /, ''))
    }
  }
  if (dataLines.length === 0) return null
  const data = dataLines.join('\n')

  switch (event) {
    case 'token':
    case 'tool':
    case 'done':
    case 'error':
      try {
        return { event, data: JSON.parse(data) } as StreamFrame
      } catch {
        return { event: 'error', data: { message: `malformed ${event} frame` } }
      }
    case 'ping':
      return { event: 'ping', data: null }
    default:
      return null // unknown event types are ignored, not fatal
  }
}

/**
 * POST `body` to `url` and stream the SSE response through `onFrame`.
 * Resolves when the stream ends; rejects on network failure or a
 * non-2xx response (JSON error envelope parsed when possible).
 */
export async function streamSse(
  url: string,
  body: unknown,
  onFrame: (frame: StreamFrame) => void,
  signal?: AbortSignal,
): Promise<void> {
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Accept: 'text/event-stream' },
    body: JSON.stringify(body),
    signal,
  })

  if (!res.ok) {
    let message = `HTTP ${res.status}`
    let code = ''
    try {
      const env = await res.json()
      message = env?.error?.message ?? message
      code = env?.error?.code ?? ''
    } catch {
      /* non-JSON error body */
    }
    throw new SseHttpError(message, res.status, code)
  }
  if (!res.body) throw new SseHttpError('response has no body', res.status, '')

  const reader = res.body.pipeThrough(new TextDecoderStream()).getReader()
  const decoder = new SseDecoder()
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    for (const frame of decoder.push(value)) onFrame(frame)
  }
  for (const frame of decoder.end()) onFrame(frame)
}

/** HTTP-level failure before the stream opened (401 / 409 / 400 …). */
export class SseHttpError extends Error {
  constructor(
    message: string,
    public readonly status: number,
    public readonly code: string,
  ) {
    super(message)
    this.name = 'SseHttpError'
  }
}
