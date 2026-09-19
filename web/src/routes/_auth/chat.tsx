import { createFileRoute, Link, Outlet, useRouterState } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'

/**
 * `/chat` layout route.
 *
 * Renders the thread sidebar once and lets the child routes
 * (`/chat/` and `/chat/$threadId`) swap only the conversation pane. This is
 * the key TanStack Router pattern: layout persistence without unmounting the
 * sidebar on every navigation.
 */
export const Route = createFileRoute('/_auth/chat')({
  component: ChatLayout,
})

function ChatLayout() {
  const { api } = Route.useRouteContext()
  const pathname = useRouterState({ select: (s) => s.location.pathname })

  const threads = useQuery({
    queryKey: ['threads'],
    queryFn: () => api.listThreads(),
  })

  return (
    <>
      <div className="page-head">
        <h1>Chat</h1>
        <p>Threads persist across navigation — the sidebar never re-mounts.</p>
      </div>

      <div className="chat-layout">
        <div className="thread-list">
          {threads.isLoading ? (
            <div className="loading">Loading…</div>
          ) : (
            threads.data?.map((t) => {
              const active = pathname === `/chat/${t.id}`
              return (
                <Link
                  key={t.id}
                  to="/chat/$threadId"
                  params={{ threadId: t.id }}
                  className={`thread-item${active ? ' active' : ''}`}
                >
                  <div className="t">{t.title}</div>
                  <div className="m">
                    {t.messageCount} messages · {t.workspaceId}
                  </div>
                </Link>
              )
            })
          )}
        </div>

        <Outlet />
      </div>
    </>
  )
}
