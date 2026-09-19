import { createFileRoute, redirect, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { z } from 'zod'

/** Post-login redirect target, validated & typed. */
const searchSchema = z.object({
  redirect: z.string().optional().catch(undefined),
})

export const Route = createFileRoute('/login')({
  validateSearch: searchSchema,
  beforeLoad: ({ context, search }) => {
    // Already signed in? Skip the login screen.
    if (context.auth.isAuthenticated) {
      throw redirect({ to: search.redirect ?? '/dashboard' })
    }
  },
  component: LoginPage,
})

function LoginPage() {
  const { auth } = Route.useRouteContext()
  const { redirect: redirectTo } = Route.useSearch()
  const navigate = useNavigate()

  const [username, setUsername] = useState('kan')
  const [role, setRole] = useState<'admin' | 'user'>('admin')

  function submit(e: React.FormEvent) {
    e.preventDefault()
    auth.login(username.trim() || 'guest', role)
    void navigate({ to: redirectTo ?? '/dashboard' })
  }

  return (
    <div className="login-wrap">
      <form className="login-card" onSubmit={submit}>
        <h1>🦊 RustFox Portal</h1>
        <p>Sign in to manage workspaces, agents and memory.</p>

        <div className="field">
          <label htmlFor="username">Username</label>
          <input
            id="username"
            type="text"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            placeholder="kan"
            autoComplete="username"
          />
        </div>

        <div className="field">
          <label htmlFor="role">Role</label>
          <select
            id="role"
            value={role}
            onChange={(e) => setRole(e.target.value as 'admin' | 'user')}
          >
            <option value="admin">admin — full access</option>
            <option value="user">user — chat only</option>
          </select>
        </div>

        <button type="submit" className="primary" style={{ width: '100%' }}>
          Sign in
        </button>

        <div className="hint">
          <strong>Prototype auth.</strong> No backend call yet — the session is
          stored in <code>localStorage</code>. Pick <code>user</code> to see the
          RBAC guard block <code>/settings</code>.
        </div>
      </form>
    </div>
  )
}
