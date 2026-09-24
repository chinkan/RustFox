import { createFileRoute, notFound } from '@tanstack/react-router'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useChatSession } from '../../hooks/useChatStream'

/**
 * `/chat/$threadId` — the live conversation pane.
 *
 * The loader checks the requested thread id against the server's active web
 * conversation (one API call, no message fetch — ChatSession owns that to
 * keep the SSE lifecycle in a single place). Unknown ids render notFound.
 * The pane itself is driven by the shared ChatSession store: optimistic user
 * bubble, streamed tokens, tool notes, stop button, and reconcile-on-restart.
 */
export const Route = createFileRoute('/_auth/chat/$threadId')({
  loader: async ({ context, params }) => {
    const threads = await context.api.listThreads()
    const thread = threads.find((t) => t.id === params.threadId)
    if (!thread) throw notFound()
    return { thread }
  },
  component: ThreadPage,
})

function ThreadPage() {
  const { thread } = Route.useLoaderData()
  const { chat } = Route.useRouteContext()
  const { t } = useTranslation()
  const { state, send, stop } = useChatSession(chat)
  const [draft, setDraft] = useState('')
  const bodyRef = useRef<HTMLDivElement>(null)

  // Initial history load (idempotent — guarded inside the session by `loaded`).
  useEffect(() => {
    if (!state.loaded) void chat.load()
  }, [chat, state.loaded])

  // Reconcile when this view mounts after a restart the watcher caught while
  // we were elsewhere (streaming flag stuck / bubbles stale).
  useEffect(() => {
    if (state.loaded && !state.streaming && state.messages.length === 0) {
      void chat.load()
    }
  }, [chat, state.loaded, state.streaming, state.messages.length])

  // Auto-scroll on stream growth.
  useEffect(() => {
    const el = bodyRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [state.messages])

  function submit(e: React.FormEvent) {
    e.preventDefault()
    const text = draft
    if (!text.trim() || state.streaming) return
    setDraft('')
    send(text)
  }

  return (
    <div className="chat-pane">
      <div className="row" style={{ padding: '13px 18px', borderBottom: '1px solid var(--border)' }}>
        <strong>{thread.title}</strong>
        <span className="spacer" />
        {state.streaming ? <span className="badge dot running">{t('chat.threadRunning')}</span> : null}
        <span className="badge">{thread.messageCount} messages</span>
      </div>

      <div className="chat-body" ref={bodyRef}>
        {!state.loaded ? (
          <div className="loading">{t('common.loading')}</div>
        ) : state.messages.length === 0 ? (
          <div className="empty">
            <div className="big">💬</div>
            <p>{t('chat.noMessages')}</p>
          </div>
        ) : (
          state.messages.map((m) => (
            <div key={m.id} className={`msg ${m.role}${m.state === 'failed' ? ' failed' : ''}`}>
              <div className="bubble">
                {m.tools.length > 0 && (
                  <div className="tool-notes">🛠 {m.tools.join(' → ')}</div>
                )}
                <span className="md">{m.content}</span>
                {m.state === 'streaming' ? <span className="cursor" /> : null}
              </div>
            </div>
          ))
        )}
        {state.error ? (
          <div className="error-note">
            {state.error === 'busy' ? t('chat.busy') : state.error}
          </div>
        ) : null}
      </div>

      <form className="chat-input" onSubmit={submit}>
        <input
          type="text"
          placeholder={t('chat.placeholder')}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          disabled={state.streaming}
        />
        {state.streaming ? (
          <button type="button" className="ghost" onClick={() => void stop()}>
            {t('chat.stop')}
          </button>
        ) : (
          <button type="submit" className="primary" disabled={!draft.trim()}>
            {t('chat.send')}
          </button>
        )}
      </form>
    </div>
  )
}
