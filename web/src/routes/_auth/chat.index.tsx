import { createFileRoute, redirect } from '@tanstack/react-router'

/**
 * `/chat/` — hop into the active web conversation.
 *
 * GET /api/chat/history creates (or returns) the current conversation, so by
 * the time this loader resolves there is always a thread id to navigate to.
 * No message fetch happens here — ChatSession owns that (and must, to keep
 * the SSE lifecycle in one place).
 */
export const Route = createFileRoute('/_auth/chat/')({
  loader: async ({ context }) => {
    const hist = await context.api.getHistory()
    throw redirect({
      to: '/chat/$threadId',
      params: { threadId: hist.conversationId },
      replace: true,
    })
  },
})
