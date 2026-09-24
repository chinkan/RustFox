import { describe, expect, it, vi } from 'vitest'
import { ChatSession } from './useChatStream'
import { ApiError, type Api } from '../api/client'
import type { StreamFrame } from '../api/types'

/**
 * ChatSession lifecycle against a scripted fake api (ADR 0008B reconcile
 * path). Uses the REAL SseDecoder frame shape — frames arrive already
 * parsed because chatStream is the seam we replace.
 */

function apiWithStream(script: StreamFrame[], opts: { throwOnChat?: Error; history?: number } = {}) {
  let historyCalls = 0
  const api = {
    getHistory: async () => {
      historyCalls += 1
      return {
        conversationId: 'conv-x',
        messages:
          historyCalls === 1
            ? []
            : [{ id: 'h0', role: 'assistant' as const, content: 'persisted answer', createdAt: null }],
      }
    },
    listThreads: async () => [],
    chatStream: async (_text: string, onFrame: (f: StreamFrame) => void) => {
      if (opts.throwOnChat) throw opts.throwOnChat
      for (const f of script) onFrame(f)
    },
    cancelChat: async () => ({ cancelled: true }),
  } as unknown as Api
  return { api, historyCallCount: () => historyCalls }
}

function snapshot(s: ChatSession) {
  return s.getSnapshot()
}

describe('ChatSession streaming', () => {
  it('streams tokens then commits the done content', async () => {
    const { api } = apiWithStream([
      { event: 'token', data: { delta: 'hel' } },
      { event: 'token', data: { delta: 'lo' } },
      { event: 'tool', data: { name: 'web_search', status: 'started' } },
      { event: 'done', data: { content: 'hello world' } },
    ])
    const s = new ChatSession(api)
    await s.send('hi')
    const st = snapshot(s)
    expect(st.streaming).toBe(false)
    const asst = st.messages.find((m) => m.role === 'assistant')!
    expect(asst.content).toBe('hello world')
    expect(asst.state).toBe('complete')
    expect(asst.tools).toEqual(['web_search'])
    expect(st.messages.find((m) => m.role === 'user')!.content).toBe('hi')
  })

  it('409 busy error keeps the user bubble and surfaces the busy code', async () => {
    // Same shape the real client rethrows after SseHttpError normalisation.
    const err = new ApiError('A generation is already running', 409, 'chat_in_progress')
    const { api } = apiWithStream([], { throwOnChat: err })
    const s = new ChatSession(api)
    await s.send('hi')
    const st = snapshot(s)
    expect(st.error).toBe('busy')
    expect(st.streaming).toBe(false)
    expect(st.messages.map((m) => m.role)).toEqual(['user'])
  })

  it('drops to reconcile when the stream closes without a terminal frame', async () => {
    // chatStream returns normally but emits nothing → server died silently.
    // Real backends persist user+assistant rows, so post-reconcile the list
    // is exactly what the DB says (optimistic bubbles replaced by truth).
    const api = {
      getHistory: async () => ({
        conversationId: 'c',
        messages: [
          { id: 'h0', role: 'user' as const, content: 'hi', createdAt: null },
          { id: 'h1', role: 'assistant' as const, content: 'persisted answer', createdAt: null },
        ],
      }),
      chatStream: async () => undefined,
      cancelChat: async () => ({ cancelled: true }),
    } as unknown as Api
    const s = new ChatSession(api)
    await s.send('hi')
    const st = snapshot(s)
    expect(st.messages.map((m) => m.content)).toEqual(['hi', 'persisted answer'])
    expect(st.messages.every((m) => m.state === 'complete')).toBe(true)
    expect(st.streaming).toBe(false)
  })

  it('never auto-replays: reconcile calls getHistory, not chatStream again', async () => {
    const streamCalls = vi.fn()
    const api = {
      getHistory: async () => ({
        conversationId: 'c',
        messages: [{ id: 'x', role: 'assistant' as const, content: 'saved', createdAt: null }],
      }),
      chatStream: async (_t: string, onFrame: (f: StreamFrame) => void) => {
        void onFrame
        streamCalls()
        // die immediately, no frames
      },
      cancelChat: async () => ({ cancelled: false }),
    } as unknown as Api
    const s = new ChatSession(api)
    await s.send('hi')
    expect(streamCalls).toHaveBeenCalledTimes(1) // never re-POSTed
    expect(snapshot(s).messages[0].content).toBe('saved')
  })

  it('ignores concurrent sends while streaming', async () => {
    let release: () => void = () => undefined
    const gate = new Promise<void>((r) => (release = r))
    const api = {
      getHistory: async () => ({ conversationId: 'c', messages: [] }),
      chatStream: async (_t: string, onFrame: (f: StreamFrame) => void) => {
        void onFrame
        onFrame({ event: 'token', data: { delta: 'x' } })
        await gate
        onFrame({ event: 'done', data: { content: 'x' } })
      },
      cancelChat: async () => ({ cancelled: true }),
    } as unknown as Api
    const s = new ChatSession(api)
    const first = s.send('one')
    await new Promise((r) => setTimeout(r, 0))
    const second = s.send('two') // must no-op
    release()
    await Promise.all([first, second])
    const st = snapshot(s)
    expect(st.messages.filter((m) => m.role === 'user').map((m) => m.content)).toEqual(['one'])
  })
})
