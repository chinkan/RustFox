import { useState } from 'react'
import { createFileRoute, Link, Outlet, useNavigate, useRouterState } from '@tanstack/react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { useChatSession } from '../../hooks/useChatStream'
import { usePhoneViewport } from '../../layout/phoneViewport'

/**
 * `/chat` layout route.
 *
 * Sidebar lists the active web conversation and archived ones. 「開新對話」
 * archives the active thread when it has messages and opens a fresh one.
 */
export const Route = createFileRoute('/_auth/chat')({
  component: ChatLayout,
})

function ChatLayout() {
  const { api, chat } = Route.useRouteContext()
  const { t } = useTranslation()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const pathname = useRouterState({ select: (s) => s.location.pathname })
  const { state: session } = useChatSession(chat)
  const [creating, setCreating] = useState(false)
  const phone = usePhoneViewport()

  const threads = useQuery({
    queryKey: ['threads'],
    queryFn: () => api.listThreads(),
  })

  const rows = threads.data ?? []

  async function onNewConversation() {
    if (session.streaming || creating) return
    setCreating(true)
    try {
      const created = await api.createThread()
      await queryClient.invalidateQueries({ queryKey: ['threads'] })
      await chat.load()
      await navigate({ to: '/chat/$threadId', params: { threadId: created.id } })
    } catch {
      /* 409 while generating, or a network miss: stay on the current thread. */
    } finally {
      setCreating(false)
    }
  }

  return (
    <>
      <div className="page-head">
        <h1>{t('chat.title')}</h1>
        <p>{t('chat.subtitle')}</p>
      </div>

      <div className="chat-layout" data-layout={phone ? 'phone' : 'desktop'}>
        <div className="thread-list">
          <button
            type="button"
            className="primary new-conversation"
            disabled={session.streaming || creating}
            onClick={() => void onNewConversation()}
          >
            {t('chat.newConversation')}
          </button>
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
                    {!th.archived && session.streaming ? ` · ${t('chat.threadRunning')}` : ''}
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
