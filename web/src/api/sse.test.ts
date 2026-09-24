import { describe, expect, it, vi } from 'vitest'
import { SseDecoder, SseHttpError, streamSse } from './sse'
import type { StreamFrame } from './types'

describe('SseDecoder', () => {
  it('parses token / tool / done frames', () => {
    const d = new SseDecoder()
    const frames = d.push(
      [
        'event: token',
        'data: {"delta":"hi"}',
        '',
        'event: tool',
        'data: {"name":"web_search","status":"started"}',
        '',
        'event: done',
        'data: {"content":"hi there"}',
        '',
        '',
      ].join('\n'),
    )
    expect(frames).toEqual([
      { event: 'token', data: { delta: 'hi' } },
      { event: 'tool', data: { name: 'web_search', status: 'started' } },
      { event: 'done', data: { content: 'hi there' } },
    ] as StreamFrame[])
  })

  it('handles chunks split across frame boundaries', () => {
    const d = new SseDecoder()
    const a = d.push('event: tok')
    const b = d.push('en\ndata: {"del')
    const c = d.push('ta":"x"}\n\n')
    expect([...a, ...b]).toEqual([])
    expect(c).toEqual([{ event: 'token', data: { delta: 'x' } }])
  })

  it('normalises CRLF and ignores comments/pings', () => {
    const d = new SseDecoder()
    const frames = d.push(': keepalive\r\n\r\nevent: ping\r\ndata: ping\r\n\r\nevent: token\r\ndata: {"delta":"y"}\r\n\r\n')
    expect(frames).toEqual([
      { event: 'ping', data: null },
      { event: 'token', data: { delta: 'y' } },
    ])
  })

  it('flushes a trailing frame on end()', () => {
    const d = new SseDecoder()
    expect(d.push('event: done\ndata: {"content":"tail"}')).toEqual([])
    expect(d.end()).toEqual([{ event: 'done', data: { content: 'tail' } }])
  })

  it('survives malformed JSON as an error frame', () => {
    const d = new SseDecoder()
    const frames = d.push('event: done\ndata: {oops\n\n')
    expect(frames).toEqual([{ event: 'error', data: { message: 'malformed done frame' } }])
  })

  it('ignores unknown event types', () => {
    const d = new SseDecoder()
    expect(d.push('event: future-thing\ndata: {}\n\n')).toEqual([])
  })
})

function mockFetchResponse(body: ReadableStream<Uint8Array>, ok = true, status = 200, json?: unknown) {
  return vi.fn().mockResolvedValue({
    ok,
    status,
    body,
    json: async () => json,
    text: async () => JSON.stringify(json ?? null),
  })
}

describe('streamSse', () => {
  it('streams frames to the callback and resolves at EOF', async () => {
    const enc = new TextEncoder()
    const stream = new ReadableStream<Uint8Array>({
      start(c) {
        c.enqueue(enc.encode('event: token\ndata: {"delta":"a"}\n\n'))
        c.enqueue(enc.encode('event: done\ndata: {"content":"ab"}\n\n'))
        c.close()
      },
    })
    const prev = globalThis.fetch
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    globalThis.fetch = mockFetchResponse(stream) as any
    const seen: StreamFrame[] = []
    try {
      await streamSse('/api/chat', { text: 'hi' }, (f) => seen.push(f))
    } finally {
      globalThis.fetch = prev
    }
    expect(seen).toEqual([
      { event: 'token', data: { delta: 'a' } },
      { event: 'done', data: { content: 'ab' } },
    ])
  })

  it('throws SseHttpError with code from the error envelope on non-2xx', async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(c) {
        c.close()
      },
    })
    const prev = globalThis.fetch
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    globalThis.fetch = mockFetchResponse(stream, false, 409, {
      error: { code: 'chat_in_progress', message: 'A generation is already running' },
    }) as unknown as typeof fetch
    try {
      await expect(streamSse('/api/chat', {}, () => undefined)).rejects.toThrow(
        SseHttpError,
      )
      await expect(streamSse('/api/chat', {}, () => undefined)).rejects.toMatchObject({
        status: 409,
        code: 'chat_in_progress',
        message: 'A generation is already running',
      })
    } finally {
      globalThis.fetch = prev
    }
  })
})
