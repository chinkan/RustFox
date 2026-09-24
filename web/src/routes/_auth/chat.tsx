import { createFileRoute, Link, Outlet, useRouterState } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { useChatSession } from '../../hooks/useChatStream'

/**
 * `/chat` layout route.
 *
 * Renders the thread sidebar once; children swap only the conversation pane.
 * MVP backend keeps a single active web conversation (docs/portal-api.md),
 * so the list normally has one row — it is still fetched from the API rather
 * than invented client-side.
 */
export const Route = createFileRoute('/_auth/chat')({
  component: ChatLayout,
})

function ChatLayout() {
  const { api, chat } = Route.useRouteContext()
  const { t } = useTranslation()
  const pathname = useRouterState({ select: (s) => s.location.pathname })
  const { state: session } = useChatSession(chat)

  const threads = useQuery({
    queryKey: ['threads'],
    queryFn: () => api.listThreads(),
  })

  const rows = threads.data ?? []

  return (
    <>
      <div className="page-head">
        <h1>{t('chat.title')}</h1>
        <p>{t('chat.subtitle')}</p>
      </div>

      <div className="chat-layout">
        <div className="thread-list">
          {threads.isLoading ? (
            <div className="loading">{t('common.loading')}</div>
          ) : (
            rows.map((th) => {
              const active = pathname === `/chat/${th.id}`
              return (
                <Link
                  key={th.id}
                  to="/chat/$threadId"
                  params={{ threadId: th.id }}
                  className={`thread-item${active ? ' active' : ''}`}
                >
                  <div className="t">{th.title}</div>
                  <div className="m">
                    {th.messageCount} messages
                    {active && session.streaming ? ` · ${t('chat.threadRunning')}` : ''}
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
