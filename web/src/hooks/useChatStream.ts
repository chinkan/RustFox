import { useCallback, useSyncExternalStore } from 'react'
import { ApiError, type Api } from '../api/client'
import type { StreamFrame } from '../api/types'

/**
 * Chat session store driving POST /api/chat SSE (ADR 0005 + 0008B).
 *
 * MVP backend keeps ONE active conversation per portal user (docs/portal-api.md
 * → GET /api/chat/history), so this store manages a single thread; the thread
 * sidebar lists it. Multi-conversation browsing is an M3 backend follow-up.
 *
 * - optimistic user bubble → token deltas appended live → `done` commits
 * - `tool` frames surface as notes on the streaming bubble
 * - mid-stream connection drop (restart) ⇒ bubble marked failed and
 *   `reconcile()` pulls the persisted history (the server only persists the
 *   final answer — truth from DB, never an auto-replay of the POST)
 *
 * Module-level store (not a plain hook) because the sidebar badge and the
 * chat pane observe the same state.
 */

export interface UiMessage {
  id: string
  role: 'user' | 'assistant'
  content: string
  state: 'complete' | 'streaming' | 'failed'
  tools: string[]
}

export interface ChatState {
  conversationId: string
  messages: UiMessage[]
  streaming: boolean
  loaded: boolean
  error: string | null
}

let counter = 0
const nextId = () => `local-${++counter}-${Date.now().toString(36)}`

export class ChatSession {
  private state: ChatState
  private listeners = new Set<() => void>()
  private controller: AbortController | null = null
  private reconciling = false

  constructor(private api: Api) {
    this.state = {
      conversationId: '',
      messages: [],
      streaming: false,
      loaded: false,
      error: null,
    }
  }

  subscribe = (l: () => void) => {
    this.listeners.add(l)
    return () => this.listeners.delete(l)
  }
  getSnapshot = () => this.state

  private set(patch: Partial<ChatState>) {
    this.state = { ...this.state, ...patch }
    this.listeners.forEach((l) => l())
  }

  private setMessages(fn: (m: UiMessage[]) => UiMessage[]) {
    this.set({ messages: fn(this.state.messages) })
  }

  /** Load history from the DB (safe to call on boot and every reconcile). */
  async load(): Promise<void> {
    try {
      const hist = await this.api.getHistory()
      this.set({
        conversationId: hist.conversationId,
        messages: hist.messages.map((m) => ({
          id: m.id,
          role: m.role,
          content: m.content,
          state: 'complete' as const,
          tools: [],
        })),
        loaded: true,
        error: null,
      })
    } catch (e) {
      this.set({ loaded: true, error: e instanceof Error ? e.message : 'load failed' })
    }
  }

  /** After a detected restart / dropped stream: truth-from-DB, no replay. */
  async reconcile(): Promise<void> {
    if (this.reconciling) return
    this.reconciling = true
    try {
      if (this.state.streaming) {
        // The stream died with the server — clear busy; load() below replaces
        // bubbles with whatever was actually persisted.
        this.set({ streaming: false })
      }
      await this.load()
    } finally {
      this.reconciling = false
    }
  }

  /** Send a message and consume the SSE stream. */
  async send(text: string): Promise<void> {
    const trimmed = text.trim()
    if (!trimmed || this.state.streaming) return

    const userMsg: UiMessage = {
      id: nextId(),
      role: 'user',
      content: trimmed,
      state: 'complete',
      tools: [],
    }
    const draftId = nextId()
    const draft: UiMessage = {
      id: draftId,
      role: 'assistant',
      content: '',
      state: 'streaming',
      tools: [],
    }
    this.setMessages((m) => [...m, userMsg, draft])
    this.set({ streaming: true, error: null })

    this.controller = new AbortController()
    let arrivedTerminal = false

    const patchDraft = (fn: (d: UiMessage) => UiMessage) =>
      this.setMessages((list) => list.map((m) => (m.id === draftId ? fn(m) : m)))

    try {
      await this.api.chatStream(
        trimmed,
        (frame: StreamFrame) => {
          switch (frame.event) {
            case 'token':
              patchDraft((d) => ({ ...d, content: d.content + frame.data.delta }))
              break
            case 'tool': {
              const name = frame.data.name
              if (name && frame.data.status === 'started') {
                patchDraft((d) => ({ ...d, tools: [...d.tools, name] }))
              }
              break
            }
            case 'done':
              arrivedTerminal = true
              patchDraft((d) => ({
                ...d,
                content: frame.data.content || d.content,
                state: 'complete',
              }))
              break
            case 'error':
              arrivedTerminal = true
              patchDraft((d) => ({ ...d, state: 'failed' }))
              this.set({ error: frame.data.message })
              break
            case 'ping':
              break // keep-alive
          }
        },
        this.controller.signal,
      )
      // Stream closed cleanly without a done/error frame ⇒ server died.
      if (!arrivedTerminal) {
        patchDraft((d) => (d.state === 'streaming' ? { ...d, state: 'failed' } : d))
        await this.reconcile()
      }
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') {
        patchDraft((d) => ({ ...d, state: 'failed' }))
      } else {
        const busy = e instanceof ApiError && e.code === 'chat_in_progress'
        // Drop the draft entirely if nothing streamed; keep partial text as failed.
        this.setMessages((list) => list.filter((m) => !(m.id === draftId && !m.content)))
        if (this.state.messages.some((m) => m.id === draftId)) {
          patchDraft((d) => ({ ...d, state: 'failed' }))
        }
        this.set({ error: busy ? 'busy' : e instanceof Error ? e.message : 'stream failed' })
        if (!busy && !arrivedTerminal) await this.reconcile()
      }
    } finally {
      this.controller = null
      this.set({ streaming: false })
    }
  }

  /** Stop button: ask the server to cancel at the next boundary. */
  async stop(): Promise<void> {
    try {
      await this.api.cancelChat()
    } catch {
      /* best effort — abort below still closes our view */
    }
    this.controller?.abort()
  }
}

/** React binding. */
export function useChatSession(session: ChatSession) {
  const state = useSyncExternalStore(session.subscribe, session.getSnapshot, session.getSnapshot)
  const send = useCallback(
    (text: string) => {
      void session.send(text)
    },
    [session],
  )
  const stop = useCallback(() => {
    void session.stop()
  }, [session])
  return { state, send, stop, session }
}

let singleton: ChatSession | null = null
export function chatSession(api: Api): ChatSession {
  if (!singleton) singleton = new ChatSession(api)
  return singleton
}
/** Test hook */
export function __resetChatSingleton() {
  singleton = null
}
