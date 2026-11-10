import { createFileRoute, redirect } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { ensureAuthBootstrapped } from '../../auth'
import { McpSettingsSection } from '../../settings/mcpSettingsSection'

/**
 * `/mcp` — admin-only. Preset and custom MCP server portal controls.
 */
export const Route = createFileRoute('/_auth/mcp')({
  beforeLoad: async ({ context }) => {
    await ensureAuthBootstrapped()
    if (!context.auth.isAdmin) {
      throw redirect({ to: '/dashboard' })
    }
  },
  component: McpPage,
})

function McpPage() {
  const { api } = Route.useRouteContext()
  const { t } = useTranslation()
  const settings = useQuery({ queryKey: ['settings'], queryFn: api.getSettings })
  const masked = settings.data?.masked

  return (
    <>
      <div className="page-head">
        <h1>{t('mcpPage.title')}</h1>
        <p>{t('mcpPage.subtitle')}</p>
      </div>

      {settings.isLoading ? (
        <div className="loading">{t('common.loading')}</div>
      ) : settings.isError ? (
        <div className="error-note">{(settings.error as Error).message}</div>
      ) : (
        <McpSettingsSection api={api} servers={masked?.mcpServers ?? []} hideTitle />
      )}
    </>
  )
}
