import { createFileRoute, Link, Outlet, redirect, useRouterState } from '@tanstack/react-router'
import { useAuth } from '../auth'

/**
 * `_auth` — the authenticated shell.
 *
 * The underscore prefix makes this a *pathless* layout route: it wraps every
 * child in the sidebar shell without contributing a URL segment. The
 * `beforeLoad` guard redirects unauthenticated visitors to /login and passes
 * the current URL along so they land back where they wanted after signing in.
 */
export const Route = createFileRoute('/_auth')({
  beforeLoad: ({ context, location }) => {
    if (!context.auth.isAuthenticated) {
      throw redirect({
        to: '/login',
        search: { redirect: location.href },
      })
    }
  },
  component: AuthLayout,
})

interface NavItem {
  to: string
  label: string
  icon: string
  adminOnly?: boolean
}

const WORKSPACE_NAV: NavItem[] = [
  { to: '/dashboard', label: 'Dashboard', icon: '📊' },
  { to: '/chat', label: 'Chat', icon: '💬' },
  { to: '/agents', label: 'Agents', icon: '🤖' },
  { to: '/memory', label: 'Memory', icon: '📚' },
  { to: '/tasks', label: 'Scheduled Tasks', icon: '⏰' },
]

const SYSTEM_NAV: NavItem[] = [
  { to: '/settings', label: 'Settings', icon: '⚙️', adminOnly: true },
]

function AuthLayout() {
  const auth = useAuth()
  const pathname = useRouterState({ select: (s) => s.location.pathname })

  const isActive = (to: string) => pathname === to || pathname.startsWith(to + '/')

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="fox">🦊</span>
          <span>
            RustFox
            <small>Web Portal</small>
          </span>
        </div>

        <nav className="nav-group">
          <div className="nav-label">Workspace</div>
          {WORKSPACE_NAV.map((item) => (
            <Link
              key={item.to}
              to={item.to}
              className={`nav-item${isActive(item.to) ? ' active' : ''}`}
            >
              <span>{item.icon}</span>
              {item.label}
            </Link>
          ))}
        </nav>

        <nav className="nav-group">
          <div className="nav-label">System</div>
          {SYSTEM_NAV.filter((i) => !i.adminOnly || auth.isAdmin).map((item) => (
            <Link
              key={item.to}
              to={item.to}
              className={`nav-item${isActive(item.to) ? ' active' : ''}`}
            >
              <span>{item.icon}</span>
              {item.label}
            </Link>
          ))}
        </nav>

        <div className="sidebar-footer">
          <div className="who">
            <span className="avatar">
              {auth.session?.username.slice(0, 1).toUpperCase() ?? '?'}
            </span>
            <span style={{ flex: 1, minWidth: 0 }}>
              <div
                style={{
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace: 'nowrap',
                }}
              >
                {auth.session?.username}
              </div>
              <span className={`badge ${auth.isAdmin ? 'admin' : ''}`} style={{ marginTop: 2 }}>
                {auth.session?.role}
              </span>
            </span>
          </div>
          <button className="ghost sm" onClick={() => auth.logout()}>
            Sign out
          </button>
        </div>
      </aside>

      <main className="main">
        <Outlet />
      </main>
    </div>
  )
}
