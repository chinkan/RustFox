import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import type { ScheduledTask, TaskCreateBody, TaskUpdateBody } from '../../api/types'
import { CRON_6_FIELD } from '../../api/types'
import { ApiError } from '../../api/client'

export const Route = createFileRoute('/_auth/tasks')({
  component: TasksPage,
})

type Draft = TaskCreateBody & TaskUpdateBody

type EditState =
  | { mode: 'view' }
  | { mode: 'create'; draft: Draft }
  | { mode: 'edit'; id: string; draft: Draft }

/** 6-field cron heuristic; the server's parser is the real gate. */
function triggerLooksValid(draft: Draft): boolean {
  const v = draft.triggerValue.trim()
  if (v === '') return false
  if (draft.triggerType === 'one_shot') {
    return !Number.isNaN(Date.parse(v)) && Date.parse(v) > Date.now()
  }
  return CRON_6_FIELD.test(v) && v.split(/\s+/).length === 6
}

function TasksPage() {
  const { api } = Route.useRouteContext()
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const tasks = useQuery({ queryKey: ['tasks'], queryFn: api.listTasks, refetchInterval: 30_000 })
  const [runsFor, setRunsFor] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [edit, setEdit] = useState<EditState>({ mode: 'view' })
  const [confirmDelete, setConfirmDelete] = useState<ScheduledTask | null>(null)

  const runs = useQuery({
    queryKey: ['task-runs', runsFor],
    queryFn: () => api.getTaskRuns(runsFor as string),
    enabled: runsFor !== null,
  })

  const invalidate = () => void queryClient.invalidateQueries({ queryKey: ['tasks'] })
  const errText = (e: unknown) =>
    e instanceof ApiError ? `${e.code}: ${e.message}` : (e as Error).message

  const toggle = useMutation<
    { message: string | null },
    Error,
    { id: string; enabled: boolean }
  >({
    mutationFn: async ({ id, enabled }) => {
      if (enabled) {
        const res = await api.enableTask(id)
        // T3: enable really re-arms — surface the live next-run, no restart fib.
        return { message: t('tasks.rearmed', { next: res.nextRun ?? '—' }) }
      }
      await api.disableTask(id)
      return { message: null }
    },
    onSuccess: (r) => {
      setNotice(r.message)
      invalidate()
    },
    onError: (e) => setNotice(errText(e)),
  })

  const create = useMutation({
    mutationFn: (body: TaskCreateBody) => api.createTask(body),
    onSuccess: (res) => {
      setNotice(t('tasks.created', { next: res.nextRun ?? '—' }))
      setEdit({ mode: 'view' })
      invalidate()
    },
    onError: (e) => setNotice(errText(e)),
  })

  const update = useMutation({
    mutationFn: ({ id, body }: { id: string; body: TaskUpdateBody }) =>
      api.updateTask(id, body),
    onSuccess: (res) => {
      setNotice(res.rearmed ? t('tasks.updatedRearmed', { next: res.nextRun ?? '—' }) : t('tasks.updated'))
      setEdit({ mode: 'view' })
      invalidate()
    },
    onError: (e) => setNotice(errText(e)),
  })

  const remove = useMutation({
    mutationFn: (id: string) => api.deleteTask(id),
    onSuccess: () => {
      setNotice(t('tasks.deleted'))
      setConfirmDelete(null)
      invalidate()
    },
    onError: (e) => setNotice(errText(e)),
  })

  function patchDraft(patch: Partial<Draft>) {
    setEdit((prev) =>
      prev.mode === 'view' ? prev : { ...prev, draft: { ...prev.draft, ...patch } },
    )
  }

  const draft = edit.mode === 'create' || edit.mode === 'edit' ? edit.draft : null
  const busy = create.isPending || update.isPending
  const formOk =
    draft !== null && draft.name.trim() !== '' && draft.prompt.trim() !== '' && triggerLooksValid(draft)

  return (
    <>
      <div className="page-head">
        <h1>{t('tasks.title')}</h1>
        <p>{t('tasks.subtitle')}</p>
      </div>

      {notice ? (
        <div className="hint" style={{ marginBottom: 12 }}>
          {notice}
        </div>
      ) : null}

      <div className="row" style={{ marginBottom: 12 }}>
        <span className="spacer" />
        {edit.mode === 'view' ? (
          <button
            className="ghost sm"
            onClick={() => {
              setNotice(null)
              setEdit({
                mode: 'create',
                draft: { name: '', prompt: '', triggerType: 'recurring', triggerValue: '0 0 12 * * *' },
              })
            }}
          >
            {t('tasks.newTask')}
          </button>
        ) : (
          <button className="ghost sm" onClick={() => setEdit({ mode: 'view' })}>
            {t('common.cancel')}
          </button>
        )}
      </div>

      {draft ? (
        <section className="section">
          <h2 className="section-title">
            {edit.mode === 'create' ? t('tasks.newTask') : t('tasks.editTask')}
          </h2>
          <div className="card">
            <div className="field">
              <label htmlFor="task-name">{t('tasks.col.task')}</label>
              <input
                id="task-name"
                type="text"
                value={draft.name}
                onChange={(e) => patchDraft({ name: e.target.value })}
              />
            </div>
            <div className="field">
              <label>{t('tasks.triggerType')}</label>
              <select
                value={draft.triggerType}
                disabled={edit.mode === 'edit'}
                onChange={(e) =>
                  patchDraft({ triggerType: e.target.value as 'recurring' | 'one_shot' })
                }
              >
                <option value="recurring">{t('tasks.recurring')}</option>
                <option value="one_shot">{t('tasks.oneShot')}</option>
              </select>
              {edit.mode === 'edit' ? <span className="hint">{t('tasks.triggerTypeLocked')}</span> : null}
            </div>
            <div className="field">
              <label>
                {draft.triggerType === 'recurring' ? t('tasks.cronExpr') : t('tasks.runAt')}
              </label>
              <input
                type="text"
                value={draft.triggerValue}
                onChange={(e) => patchDraft({ triggerValue: e.target.value })}
                placeholder={draft.triggerType === 'recurring' ? '0 30 7 * * *' : '2026-09-26T20:00:00+08:00'}
              />
              {triggerLooksValid(draft) ? (
                <span className="hint">
                  {draft.triggerType === 'recurring' ? t('tasks.cronHint') : ''}
                </span>
              ) : (
                <span className="error-note">{t('tasks.triggerInvalid')}</span>
              )}
            </div>
            <div className="field">
              <label htmlFor="task-prompt">{t('tasks.prompt')}</label>
              <textarea
                id="task-prompt"
                rows={4}
                value={draft.prompt}
                onChange={(e) => patchDraft({ prompt: e.target.value })}
                style={{ width: '100%', fontFamily: 'var(--mono)', fontSize: 12.5 }}
              />
            </div>
            <div className="row">
              <button
                className="primary"
                disabled={!formOk || busy}
                onClick={() => {
                  if (edit.mode === 'create') {
                    create.mutate({
                      name: draft.name,
                      prompt: draft.prompt,
                      triggerType: draft.triggerType,
                      triggerValue: draft.triggerValue,
                    })
                  } else if (edit.mode === 'edit') {
                    update.mutate({
                      id: edit.id,
                      body: {
                        name: draft.name,
                        prompt: draft.prompt,
                        triggerValue: draft.triggerValue,
                      },
                    })
                  }
                }}
              >
                {busy
                  ? t('settings.saving')
                  : edit.mode === 'create'
                    ? t('tasks.create')
                    : t('tasks.save')}
              </button>
            </div>
          </div>
        </section>
      ) : null}

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
                  <button
                    className="ghost sm"
                    onClick={() => void toggle.mutate({ id: task.id, enabled: !task.enabled })}
                  >
                    {task.enabled ? t('tasks.disable') : t('tasks.enable')}
                  </button>{' '}
                  <button
                    className="ghost sm"
                    onClick={() =>
                      setEdit({
                        mode: 'edit',
                        id: task.id,
                        draft: {
                          name: task.name,
                          prompt: task.prompt,
                          triggerType: task.triggerType as 'recurring' | 'one_shot',
                          triggerValue: task.triggerValue,
                        },
                      })
                    }
                  >
                    {t('tasks.edit')}
                  </button>{' '}
                  <button
                    className="ghost sm"
                    onClick={() => setRunsFor(runsFor === task.id ? null : task.id)}
                  >
                    {runsFor === task.id ? t('tasks.hideRuns') : t('tasks.runs')}
                  </button>{' '}
                  <button className="ghost sm" onClick={() => setConfirmDelete(task)}>
                    {t('tasks.delete')}
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

      {confirmDelete ? (
        <div className="modal-backdrop" onClick={() => setConfirmDelete(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>{t('tasks.deleteConfirmTitle')}</h3>
            <p>{t('tasks.deleteConfirm', { name: confirmDelete.name })}</p>
            <div className="row" style={{ justifyContent: 'flex-end' }}>
              <button className="ghost sm" onClick={() => setConfirmDelete(null)}>
                {t('common.cancel')}
              </button>
              <button
                className="danger sm"
                disabled={remove.isPending}
                onClick={() => remove.mutate(confirmDelete.id)}
              >
                {t('tasks.delete')}
              </button>
            </div>
          </div>
        </div>
      ) : null}

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
                      <span
                        className={`badge dot ${r.status === 'completed' ? 'running' : r.status === 'failed' ? 'error' : 'idle'}`}
                      >
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
