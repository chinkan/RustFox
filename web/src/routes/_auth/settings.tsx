import { createFileRoute, redirect } from '@tanstack/react-router'

/**
 * `/settings` — admin-only. Demonstrates RBAC at the route level: a `user`
 * who navigates here is bounced to the dashboard before any component renders.
 */
export const Route = createFileRoute('/_auth/settings')({
  beforeLoad: ({ context }) => {
    if (!context.auth.isAdmin) {
      throw redirect({ to: '/dashboard' })
    }
  },
  component: SettingsPage,
})

function SettingsPage() {
  const { auth } = Route.useRouteContext()

  return (
    <>
      <div className="page-head">
        <h1>Settings</h1>
        <p>
          Admin-only route. Signed in as <strong>{auth.session?.username}</strong>{' '}
          (<span className="mono">{auth.session?.role}</span>).
        </p>
      </div>

      <div className="grid cols-2">
        {[
          { icon: '🔌', title: 'Providers', desc: 'OpenAI, Anthropic, OpenRouter, Ollama keys' },
          { icon: '🧠', title: 'Soul files', desc: 'SOUL.md / USER.md / AGENTS.md editor' },
          { icon: '🛠️', title: 'Skills', desc: 'Enable, disable and edit SKILL.md files' },
          { icon: '🔐', title: 'Access control', desc: 'Users, roles and API keys' },
          { icon: '🌐', title: 'Static hosting', desc: 'Agent output gallery & webhooks' },
          { icon: '📤', title: 'Export / Import', desc: 'Backup workspace configuration' },
        ].map((c) => (
          <div key={c.title} className="card">
            <div className="row">
              <span style={{ fontSize: 18 }}>{c.icon}</span>
              <strong>{c.title}</strong>
            </div>
            <p style={{ color: 'var(--text-dim)', fontSize: 13, margin: '8px 0 0' }}>
              {c.desc}
            </p>
          </div>
        ))}
      </div>

      <div className="hint" style={{ marginTop: 24, borderTop: '1px solid var(--border)' }}>
        Sign in as <code>user</code> and try opening <code>/settings</code> — the{' '}
        <code>beforeLoad</code> guard redirects you away.
      </div>
    </>
  )
}
