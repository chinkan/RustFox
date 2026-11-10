import { createFileRoute, redirect } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { ensureAuthBootstrapped } from '../../auth'
import { DevicesSection } from '../../settings/devicesSection'

/**
 * `/devices` — admin-only. Device WebSocket token lifecycle (create / rotate / revoke).
 */
export const Route = createFileRoute('/_auth/devices')({
  beforeLoad: async ({ context }) => {
    await ensureAuthBootstrapped()
    if (!context.auth.isAdmin) {
      throw redirect({ to: '/dashboard' })
    }
  },
  component: DevicesPage,
})

function DevicesPage() {
  const { api } = Route.useRouteContext()
  const { t } = useTranslation()

  return (
    <>
      <div className="page-head">
        <h1>{t('devices.title')}</h1>
        <p>{t('devices.subtitle')}</p>
      </div>
      <DevicesSection api={api} hideTitle />
    </>
  )
}
