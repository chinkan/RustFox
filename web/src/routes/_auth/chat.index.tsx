import { createFileRoute, Link } from '@tanstack/react-router'

export const Route = createFileRoute('/_auth/chat/')({
  component: ChatIndex,
})

function ChatIndex() {
  return (
    <div className="chat-pane">
      <div className="empty">
        <div className="big">💬</div>
        <p>Select a thread on the left to start chatting.</p>
        <Link to="/chat/$threadId" params={{ threadId: 't1' }} className="btn primary">
          Open “Weekly vault review”
        </Link>
      </div>
    </div>
  )
}
