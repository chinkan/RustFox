import { useCallback, useSyncExternalStore } from 'react'
import { ApiError, type Api } from '../api/client'
import type { ChatMessage, StreamFrame } from '../api/types'

/**
 * Chat session store driving POST /api/chat SSE (ADR 0005 + 0008B).
 *
 * Send always targets the single active web conversation (`POST /api/chat`
 * has no conversation id). Opening an archived thread loads
 * `GET /api/chat/history?conversationId=` and refuses send, so a click in
 * an old transcript cannot append to the active one.
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
  /** True while a generation is running, including when another thread is on screen. */
  streaming: boolean
  /** Archived / non-active thread: composer must not send. */
  readOnly: boolean
  loaded: boolean
  error: string | null
}

let counter = 0
const nextId = () => `local-${++counter}-${Date.now().toString(36)}`

function toUi(messages: ChatMessage[]): UiMessage[] {
  return messages.map((m) => ({
    id: m.id,
    role: m.role,
    content: m.content,
    state: 'complete' as const,
    tools: [],
  }))
}

export class ChatSession {
  private state: ChatState
  private listeners = new Set<() => void>()
  private controller: AbortController | null = null
  private reconciling = false
  /** Active (send-target) conversation from a parameterless history load. */
  private activeId = ''
  /** Transcript currently on screen. Empty until the first load or send. */
  private viewId = ''
  private explicitReadOnly = false
  private streamingFlag = false
  /** Conversation the in-flight generation is painting. */
  private streamingOn = ''
  /** True only while `chatStream` has not settled. Reconcile runs after this drops. */
  private streamLive = false
  private transcripts = new Map<string, UiMessage[]>()
  private loadedIds = new Set<string>()
  private error: string | null = null

  constructor(private api: Api) {
    this.state = {
      conversationId: '',
      messages: [],
      streaming: false,
      readOnly: false,
      loaded: false,
      error: null,
    }
  }

  subscribe = (l: () => void) => {
    this.listeners.add(l)
    return () => this.listeners.delete(l)
  }
  getSnapshot = () => this.state

  private isReadOnly(): boolean {
    return (
      this.explicitReadOnly ||
      (this.activeId !== '' && this.viewId !== '' && this.viewId !== this.activeId)
    )
  }

  private publish() {
    const id = this.viewId
    this.state = {
      conversationId: id,
      messages: this.transcripts.get(id) ?? [],
      streaming: this.streamingFlag,
      readOnly: this.isReadOnly(),
      loaded: this.loadedIds.has(id),
      error: this.error,
    }
    this.listeners.forEach((l) => l())
  }

  /**
   * Load history. No id → active get-or-create thread (send target).
   * An id loads that conversation and is read-only when it is not the active one.
   */
  async load(conversationId?: string): Promise<void> {
    // Paint the requested thread before the fetch returns so a route switch
    // does not keep showing the previous transcript.
    const hinted = (conversationId ?? '').trim() || this.activeId
    if (hinted && hinted !== this.viewId) {
      this.viewId = hinted
      this.explicitReadOnly =
        Boolean(conversationId) && this.activeId !== '' && hinted !== this.activeId
      this.publish()
    }
    try {
      const hist = conversationId
        ? await this.api.getHistory(conversationId)
        : await this.api.getHistory()
      const id = hist.conversationId
      if (!conversationId) this.activeId = id
      const keepLive =
        this.streamLive && this.streamingOn === id && this.transcripts.has(id)
      if (!keepLive) this.transcripts.set(id, toUi(hist.messages))
      this.viewId = id
      this.explicitReadOnly = Boolean(conversationId) && this.activeId !== '' && id !== this.activeId
      this.loadedIds.add(id)
      this.error = null
    } catch (e) {
      this.error = e instanceof Error ? e.message : 'load failed'
      const id = conversationId || this.viewId
      if (id) {
        this.viewId = id
        this.loadedIds.add(id)
      }
    }
    this.publish()
  }

  /**
   * Open an archived thread. Learns the active id first so a later Send on
   * the active pane still targets it, and so this view cannot send.
   */
  async showArchived(conversationId: string): Promise<void> {
    if (this.viewId !== conversationId) {
      this.viewId = conversationId
      this.explicitReadOnly = true
      this.publish()
    }
    if (!this.activeId) {
      try {
        const active = await this.api.getHistory()
        this.activeId = active.conversationId
        if (!this.transcripts.has(active.conversationId)) {
          this.transcripts.set(active.conversationId, toUi(active.messages))
        }
        this.loadedIds.add(active.conversationId)
      } catch {
        /* The archived transcript is still shown read-only. */
      }
    }
    await this.load(conversationId)
    this.explicitReadOnly = true
    this.publish()
  }

  /** After a detected restart / dropped stream: truth-from-DB, no replay. */
  async reconcile(): Promise<void> {
    if (this.reconciling) return
    this.reconciling = true
    try {
      if (this.streamingFlag) this.streamingFlag = false
      if (this.explicitReadOnly && this.viewId) {
        await this.load(this.viewId)
        this.explicitReadOnly = true
        this.publish()
      } else {
        await this.load()
      }
    } finally {
      this.reconciling = false
    }
  }

  /** Send a message and consume the SSE stream. No-ops on a read-only thread. */
  async send(text: string): Promise<void> {
    const trimmed = text.trim()
    if (!trimmed || this.streamingFlag || this.isReadOnly()) return

    const bucket = this.viewId
    this.streamingOn = bucket
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
    this.transcripts.set(bucket, [...(this.transcripts.get(bucket) ?? []), userMsg, draft])
    this.streamingFlag = true
    this.error = null
    this.publish()

    this.controller = new AbortController()
    let arrivedTerminal = false

    const patchDraft = (fn: (d: UiMessage) => UiMessage) => {
      const list = this.transcripts.get(bucket) ?? []
      this.transcripts.set(
        bucket,
        list.map((m) => (m.id === draftId ? fn(m) : m)),
      )
      this.publish()
    }

    this.streamLive = true
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
              this.error = frame.data.message
              this.publish()
              break
            case 'ping':
              break // keep-alive
          }
        },
        this.controller.signal,
      )
      this.streamLive = false
      // Stream closed cleanly without a done/error frame ⇒ server died.
      if (!arrivedTerminal) {
        patchDraft((d) => (d.state === 'streaming' ? { ...d, state: 'failed' } : d))
        await this.reconcile()
      }
    } catch (e) {
      this.streamLive = false
      if (e instanceof DOMException && e.name === 'AbortError') {
        patchDraft((d) => ({ ...d, state: 'failed' }))
      } else {
        const busy = e instanceof ApiError && e.code === 'chat_in_progress'
        const list = (this.transcripts.get(bucket) ?? []).filter(
          (m) => !(m.id === draftId && !m.content),
        )
        this.transcripts.set(bucket, list)
        if (list.some((m) => m.id === draftId)) {
          patchDraft((d) => ({ ...d, state: 'failed' }))
        } else {
          this.publish()
        }
        this.error = busy ? 'busy' : e instanceof Error ? e.message : 'stream failed'
        this.publish()
        if (!busy && !arrivedTerminal) await this.reconcile()
      }
    } finally {
      this.streamLive = false
      this.controller = null
      this.streamingFlag = false
      this.publish()
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
