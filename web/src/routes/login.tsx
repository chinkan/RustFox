import { createFileRoute, redirect, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { z } from 'zod'
import { ensureAuthBootstrapped } from '../auth'
import { LANGS, setLang, type Lang } from '../i18n'

/** Post-login redirect target, validated & typed. */
const searchSchema = z.object({
  redirect: z.string().optional().catch(undefined),
})

export const Route = createFileRoute('/login')({
  validateSearch: searchSchema,
  beforeLoad: async ({ context, search }) => {
    await ensureAuthBootstrapped()
    // Returning visitor with a valid cookie? Skip the login screen.
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
  const { t, i18n } = useTranslation()

  const [token, setToken] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function submit(e: React.FormEvent) {
    e.preventDefault()
    const trimmed = token.trim()
    if (!trimmed || busy) return
    setBusy(true)
    setError(null)
    try {
      await auth.loginWithToken(trimmed)
      void navigate({ to: redirectTo ?? '/dashboard' })
    } catch (err) {
      // ApiError: 401 ⇒ bad token; 0/network ⇒ server unreachable.
      const status = (err as { status?: number }).status
      setError(status === 0 || status === undefined ? t('login.offline') : t('login.badToken'))
      setBusy(false)
    }
  }

  return (
    <div className="login-wrap">
      <form className="login-card" onSubmit={submit}>
        <h1>🦊 {t('login.title')}</h1>
        <p>{t('login.subtitle')}</p>

        <div className="field">
          <label htmlFor="token">{t('login.tokenLabel')}</label>
          <input
            id="token"
            type="password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder={t('login.tokenPlaceholder')}
            autoComplete="current-password"
            autoFocus
          />
        </div>

        {error ? <div className="login-error">{error}</div> : null}

        <button type="submit" className="primary" style={{ width: '100%' }} disabled={busy || !token.trim()}>
          {busy ? t('common.loading') : t('login.signIn')}
        </button>

        <div className="hint">{t('login.hint')}</div>

        <div className="lang-row">
          {LANGS.map((l) => (
            <button
              key={l}
              type="button"
              className={i18n.language === l ? 'sm primary' : 'sm'}
              onClick={() => setLang(l as Lang)}
            >
              {l === 'en' ? 'EN' : '粵'}
            </button>
          ))}
        </div>
      </form>
    </div>
  )
}
