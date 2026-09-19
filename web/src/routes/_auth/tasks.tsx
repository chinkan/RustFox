import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'

export const Route = createFileRoute('/_auth/tasks')({
  component: TasksPage,
})

function TasksPage() {
  const { api } = Route.useRouteContext()
  const tasks = useQuery({ queryKey: ['tasks'], queryFn: api.listTasks })

  return (
    <>
      <div className="page-head">
        <h1>Scheduled Tasks</h1>
        <p>Recurring agent runs — replaces editing crontab by hand.</p>
      </div>

      <div className="card" style={{ padding: 0, overflow: 'hidden' }}>
        <table>
          <thead>
            <tr>
              <th>Task</th>
              <th>Cron</th>
              <th>Next run (HKT)</th>
              <th>Status</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {tasks.data?.map((t) => (
              <tr key={t.id}>
                <td>
                  <strong>{t.name}</strong>
                </td>
                <td className="mono">{t.cron}</td>
                <td className="mono">{t.nextRun}</td>
                <td>
                  <span className={`badge dot ${t.enabled ? 'running' : 'idle'}`}>
                    {t.enabled ? 'enabled' : 'paused'}
                  </span>
                </td>
                <td style={{ textAlign: 'right' }}>
                  <button className="ghost sm">Run now</button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <p className="mono" style={{ marginTop: 16 }}>
        Prototype: “Run now” is not wired. Hook it to POST /api/tasks/:id/run.
      </p>
    </>
  )
}
