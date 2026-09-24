import { createFileRoute, Link, Outlet, redirect, useRouterState } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { useAuth } from '../auth'
import { useBootState } from '../hooks/useBootWatcher'
import { ensureAuthBootstrapped } from '../auth'
import { LANGS, setLang, type Lang } from '../i18n'

/**
 * `_auth` — the authenticated shell.
 *
 * Pathless layout route: wraps every child in the sidebar without
 * contributing a URL segment. `beforeLoad` awaits the /auth/me round-trip so
 * a returning session (HttpOnly cookie, ADR 0008A) never bounces to login on
 * cold boot; only a genuinely missing/expired cookie redirects, carrying the
 * original URL so post-login lands back where they wanted.
 */
export const Route = createFileRoute('/_auth')({
  beforeLoad: async ({ context, location }) => {
    await ensureAuthBootstrapped()
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
  labelKey: string
  icon: string
  adminOnly?: boolean
}

const WORKSPACE_NAV: NavItem[] = [
  { to: '/dashboard', labelKey: 'nav.dashboard', icon: '📊' },
  { to: '/chat', labelKey: 'nav.chat', icon: '💬' },
  { to: '/agents', labelKey: 'nav.agents', icon: '🤖' },
  { to: '/memory', labelKey: 'nav.memory', icon: '📚' },
  { to: '/tasks', labelKey: 'nav.tasks', icon: '⏰' },
]

const SYSTEM_NAV: NavItem[] = [
  { to: '/settings', labelKey: 'nav.settings', icon: '⚙️', adminOnly: true },
]

function AuthLayout() {
  const auth = useAuth()
  const { t, i18n } = useTranslation()
  const boot = useBootState(Route.useRouteContext().boot)
  const pathname = useRouterState({ select: (s) => s.location.pathname })

  const isActive = (to: string) => pathname === to || pathname.startsWith(to + '/')

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="fox">🦊</span>
          <span>
            RustFox
            <small>{t('app.tagline')}</small>
          </span>
        </div>

        <nav className="nav-group">
          <div className="nav-label">{t('nav.workspace')}</div>
          {WORKSPACE_NAV.map((item) => (
            <Link
              key={item.to}
              to={item.to}
              className={`nav-item${isActive(item.to) ? ' active' : ''}`}
            >
              <span>{item.icon}</span>
              {t(item.labelKey)}
            </Link>
          ))}
        </nav>

        <nav className="nav-group">
          <div className="nav-label">{t('nav.system')}</div>
          {SYSTEM_NAV.filter((i) => !i.adminOnly || auth.isAdmin).map((item) => (
            <Link
              key={item.to}
              to={item.to}
              className={`nav-item${isActive(item.to) ? ' active' : ''}`}
            >
              <span>{item.icon}</span>
              {t(item.labelKey)}
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
          <div className="row" style={{ gap: 8 }}>
            <div className="tag-list">
              {LANGS.map((l) => (
                <button
                  key={l}
                  className={i18n.language === l ? 'sm primary' : 'sm ghost'}
                  onClick={() => setLang(l as Lang)}
                >
                  {l === 'en' ? 'EN' : '粵'}
                </button>
              ))}
            </div>
            <span className="spacer" />
            <button className="ghost sm" onClick={() => auth.logout()}>
              {t('nav.signOut')}
            </button>
          </div>
        </div>
      </aside>

      <main className="main">
        {!boot.online ? (
          <div className="banner warn" role="status">
            {t('common.restarting')}
          </div>
        ) : null}
        <Outlet />
      </main>
    </div>
  )
}
