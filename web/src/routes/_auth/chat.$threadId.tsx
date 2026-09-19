import { createFileRoute, notFound } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'

/**
 * `/chat/$threadId` — the conversation pane.
 *
 * `loader` fetches before render, so the thread is present on first paint.
 * The loader's return value is typed onto `Route.useLoaderData()`, and any
 * thrown `notFound()` renders the route's `notFoundComponent`.
 */
export const Route = createFileRoute('/_auth/chat/$threadId')({
  loader: async ({ context, params }) => {
    const thread = await context.api.getThread(params.threadId)
    if (!thread) throw notFound()
    return { thread }
  },
  component: ThreadPage,
})

function ThreadPage() {
  const { thread } = Route.useLoaderData()
  const { api } = Route.useRouteContext()

  const messages = useQuery({
    queryKey: ['messages', thread.id],
    queryFn: () => api.getMessages(thread.id),
  })

  return (
    <div className="chat-pane">
      <div className="row" style={{ padding: '13px 18px', borderBottom: '1px solid var(--border)' }}>
        <strong>{thread.title}</strong>
        <span className="spacer" />
        <span className="badge">{thread.messageCount} messages</span>
      </div>

      <div className="chat-body">
        {messages.isLoading ? (
          <div className="loading">Loading messages…</div>
        ) : (
          messages.data?.map((m) => (
            <div key={m.id} className={`msg ${m.role}`}>
              <div className="bubble">{m.content}</div>
            </div>
          ))
        )}
      </div>

      <div className="chat-input">
        <input type="text" placeholder="Message RustFox… (prototype: not wired)" disabled />
        <button className="primary" disabled>
          Send
        </button>
      </div>
    </div>
  )
}
