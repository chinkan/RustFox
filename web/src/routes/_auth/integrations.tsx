import { createFileRoute, redirect } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { ensureAuthBootstrapped } from '../../auth'
import { GoogleConnect } from '../../settings/googleConnect'
import { SwitchBotSection } from '../../settings/switchBotSection'
import { McpSettingsSection } from '../../settings/mcpSettingsSection'

/**
 * `/integrations` — admin-only. Google connector, SwitchBot, and MCP portal controls.
 */
export const Route = createFileRoute('/_auth/integrations')({
  beforeLoad: async ({ context }) => {
    await ensureAuthBootstrapped()
    if (!context.auth.isAdmin) {
      throw redirect({ to: '/dashboard' })
    }
  },
  component: IntegrationsPage,
})

function IntegrationsPage() {
  const { api } = Route.useRouteContext()
  const { t } = useTranslation()
  const settings = useQuery({ queryKey: ['settings'], queryFn: api.getSettings })
  const masked = settings.data?.masked

  return (
    <>
      <div className="page-head">
        <h1>{t('integrations.title')}</h1>
        <p>{t('integrations.subtitle')}</p>
      </div>

      {settings.isLoading ? (
        <div className="loading">{t('common.loading')}</div>
      ) : settings.isError ? (
        <div className="error-note">{(settings.error as Error).message}</div>
      ) : (
        <div className="grid cols-2">
          <GoogleConnect api={api} />
          {settings.data?.switchbot ? (
            <SwitchBotSection api={api} switchbot={settings.data.switchbot} />
          ) : null}
          <McpSettingsSection api={api} servers={masked?.mcpServers ?? []} />
        </div>
      )}
    </>
  )
}
