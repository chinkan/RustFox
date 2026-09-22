import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'

export const Route = createFileRoute('/_auth/tasks')({
  component: TasksPage,
})

function TasksPage() {
  const { api } = Route.useRouteContext()
  const { t } = useTranslation()
  const tasks = useQuery({ queryKey: ['tasks'], queryFn: api.listTasks, refetchInterval: 30_000 })
  const [runsFor, setRunsFor] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)

  const runs = useQuery({
    queryKey: ['task-runs', runsFor],
    queryFn: () => api.getTaskRuns(runsFor as string),
    enabled: runsFor !== null,
  })

  async function toggle(id: string, enabled: boolean) {
    setNotice(null)
    try {
      const res = enabled ? await api.enableTask(id) : await api.disableTask(id)
      if (res.restartToSchedule) setNotice(t('tasks.restartNote'))
      await tasks.refetch()
    } catch (e) {
      setNotice((e as Error).message)
    }
  }

  return (
    <>
      <div className="page-head">
        <h1>{t('tasks.title')}</h1>
        <p>{t('tasks.subtitle')}</p>
      </div>

      {notice ? <div className="hint" style={{ marginBottom: 12 }}>{notice}</div> : null}

      <div className="card" style={{ padding: 0, overflow: 'hidden' }}>
        <table>
          <thead>
            <tr>
              <th>{t('tasks.col.task')}</th>
              <th>{t('tasks.col.cron')}</th>
              <th>{t('tasks.col.nextRun')}</th>
              <th>{t('tasks.col.status')}</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {tasks.data?.map((task) => (
              <tr key={task.id}>
                <td>
                  <strong>{task.name}</strong>
                </td>
                <td className="mono">{task.cron}</td>
                <td className="mono">{task.nextRun}</td>
                <td>
                  <span className={`badge dot ${task.enabled ? 'running' : 'idle'}`}>
                    {task.enabled ? t('common.enabled') : t('common.paused')}
                  </span>
                </td>
                <td style={{ textAlign: 'right', whiteSpace: 'nowrap' }}>
                  <button className="ghost sm" onClick={() => void toggle(task.id, !task.enabled)}>
                    {task.enabled ? t('tasks.disable') : t('tasks.enable')}
                  </button>{' '}
                  <button
                    className="ghost sm"
                    onClick={() => setRunsFor(runsFor === task.id ? null : task.id)}
                  >
                    {runsFor === task.id ? t('tasks.hideRuns') : t('tasks.runs')}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {tasks.data?.length === 0 ? (
          <div className="empty">
            <div className="big">⏰</div>
            <p>{t('tasks.noRuns')}</p>
          </div>
        ) : null}
      </div>

      {runsFor ? (
        <section className="section">
          <h2 className="section-title">
            {t('tasks.runs')} · <span className="mono">{runsFor.slice(0, 8)}</span>
          </h2>
          <div className="card" style={{ padding: 0, overflow: 'hidden' }}>
            <table>
              <thead>
                <tr>
                  <th>#</th>
                  <th>{t('tasks.col.nextRun')}</th>
                  <th>{t('tasks.col.status')}</th>
                  <th>Output</th>
                </tr>
              </thead>
              <tbody>
                {runs.data?.map((r) => (
                  <tr key={r.id}>
                    <td className="mono">{r.id}</td>
                    <td className="mono">{r.runAt}</td>
                    <td>
                      <span className={`badge dot ${r.status === 'completed' ? 'running' : r.status === 'failed' ? 'error' : 'idle'}`}>
                        {r.status}
                      </span>
                    </td>
                    <td style={{ fontSize: 12.5, color: 'var(--text-dim)' }}>
                      {r.error ?? (r.response ?? '').slice(0, 160)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            {runs.isLoading ? <div className="loading">{t('common.loading')}</div> : null}
            {runs.data?.length === 0 ? (
              <div className="empty">
                <p>{t('tasks.noRuns')}</p>
              </div>
            ) : null}
          </div>
        </section>
      ) : null}
    </>
  )
}
