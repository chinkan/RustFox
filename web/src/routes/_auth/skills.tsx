import { createFileRoute } from '@tanstack/react-router'
import { useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import type {
  CreateEntryResult,
  EntryDetail,
  InstallDryRunResult,
  InstallResult,
  InstalledLedgerView,
  SkillEntry,
} from '../../api/types'
import { warningKey } from '../../api/types'
import type { Api } from '../../api/client'
import { ApiError } from '../../api/client'

export const Route = createFileRoute('/_auth/skills')({
  component: SkillsPage,
})

const errText = (e: unknown) =>
  e instanceof ApiError ? `${e.code}: ${e.message}` : (e as Error).message

/** What the drawer edits: an entry + the file currently open. */
interface OpenFile {
  path: string
  content: string
  baseHash: string | null
  dirty: boolean
}

function SkillsPage() {
  const { api } = Route.useRouteContext()
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const entries = useQuery({ queryKey: ['entries'], queryFn: () => api.listEntries() })
  const [open, setOpen] = useState<{ kind: 'skill' | 'agent'; name: string } | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [confirmDelete, setConfirmDelete] = useState<SkillEntry | null>(null)
  const [newEntry, setNewEntry] = useState<'skill' | 'agent' | null>(null)
  const [installOpen, setInstallOpen] = useState(false)

  const reload = useMutation({
    mutationFn: api.reloadAgents,
    onSuccess: (r) => {
      setNotice(t('skills.reloadDone', { skills: r.skillsLoaded, agents: r.agentsLoaded }))
      invalidateAll()
    },
    onError: (e) => setNotice(errText(e)),
  })

  const remove = useMutation({
    mutationFn: (e: SkillEntry) =>
      e.kind === 'agent' ? api.deleteAgent(e.name) : api.deleteSkill(e.name),
    onSuccess: () => {
      setConfirmDelete(null)
      invalidateAll()
    },
    onError: (e) => setNotice(errText(e)),
  })

  function invalidateAll() {
    void queryClient.invalidateQueries({ queryKey: ['entries'] })
    void queryClient.invalidateQueries({ queryKey: ['installed'] })
  }

  return (
    <>
      <div className="page-head">
        <h1>{t('skills.title')}</h1>
        <p>{t('skills.subtitle')}</p>
      </div>

      {notice ? (
        <div className="hint" style={{ marginBottom: 12 }}>
          {notice}
        </div>
      ) : null}

      <div className="row" style={{ marginBottom: 12 }}>
        <button
          className="ghost sm"
          onClick={() => {
            setNewEntry('skill')
            setNotice(null)
          }}
        >
          {t('skills.newSkill')}
        </button>
        <button className="ghost sm" onClick={() => setNewEntry('agent')}>
          + {t('nav.agents')}
        </button>
        <span className="spacer" />
        <button className="ghost sm" disabled={reload.isPending} onClick={() => reload.mutate()}>
          {t('skills.reload')}
        </button>
        <button className="primary sm" onClick={() => setInstallOpen(true)}>
          {t('install.title')}
        </button>
      </div>

      <div className="card" style={{ padding: 0, overflow: 'hidden' }}>
        <table>
          <thead>
            <tr>
              <th>{t('skills.col.name')}</th>
              <th>{t('agents.col.agent')}</th>
              <th>{t('skills.col.source')}</th>
              <th>{t('skills.col.files')}</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {entries.data?.map((e) => (
              <tr key={`${e.kind}:${e.name}`}>
                <td>
                  <strong>{e.name}</strong>
                  {e.modified ? (
                    <span className="dirty-dot" title={t('skills.modifiedMark')}>
                      ●
                    </span>
                  ) : null}
                </td>
                <td>
                  <span className="badge">{e.kind}</span>
                </td>
                <td>
                  <span className={`badge prov-badge prov-${e.provenance}`}>{e.provenance}</span>
                </td>
                <td className="mono dim">—</td>
                <td style={{ textAlign: 'right', whiteSpace: 'nowrap' }}>
                  <button
                    className="ghost sm"
                    onClick={() => setOpen({ kind: e.kind, name: e.name })}
                  >
                    {t('skills.openEditor')}
                  </button>{' '}
                  {e.deletable ? (
                    <button className="ghost sm" onClick={() => setConfirmDelete(e)}>
                      {t('skills.delete')}
                    </button>
                  ) : (
                    <span className="hint" style={{ fontSize: 11.5 }}>
                      {t('skills.bundledNoDelete')}
                    </span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {entries.isLoading ? <div className="loading">{t('common.loading')}</div> : null}
      </div>

      {open ? (
        <EditorDrawer
          key={`${open.kind}:${open.name}`}
          api={api}
          kind={open.kind}
          name={open.name}
          onClose={() => setOpen(null)}
          onChanged={invalidateAll}
          notify={setNotice}
        />
      ) : null}

      {newEntry ? (
        <CreateDialog
          api={api}
          kind={newEntry}
          onClose={() => setNewEntry(null)}
          onCreated={() => {
            setNewEntry(null)
            invalidateAll()
          }}
          notify={setNotice}
        />
      ) : null}

      {installOpen ? (
        <InstallDialog
          api={api}
          onClose={() => setInstallOpen(false)}
          onInstalled={() => {
            invalidateAll()
          }}
          notify={setNotice}
        />
      ) : null}

      {confirmDelete ? (
        <div className="modal-backdrop" onClick={() => setConfirmDelete(null)}>
          <div className="modal" style={{ maxWidth: 480 }} onClick={(ev) => ev.stopPropagation()}>
            <h3>{t('skills.delete')}</h3>
            <p>{t('skills.deleteConfirm', { name: confirmDelete.name })}</p>
            <div className="row" style={{ justifyContent: 'flex-end' }}>
              <button className="ghost sm" onClick={() => setConfirmDelete(null)}>
                {t('common.cancel')}
              </button>
              <button
                className="danger sm"
                disabled={remove.isPending}
                onClick={() => remove.mutate(confirmDelete)}
              >
                {t('skills.delete')}
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </>
  )
}

// ---------------------------------------------------------------------------
// Editor drawer — file pills + textarea, optimistic lock (ADR 0011a R4)
// ---------------------------------------------------------------------------

function EditorDrawer(props: {
  api: Api
  kind: 'skill' | 'agent'
  name: string
  onClose: () => void
  onChanged: () => void
  notify: (s: string | null) => void
}) {
  const { api, kind, name, onClose, onChanged, notify } = props
  const { t } = useTranslation()
  const detail = useQuery<EntryDetail>({
    queryKey: ['entry', kind, name],
    queryFn: () => (kind === 'agent' ? api.getAgentDetail(name) : api.getSkill(name)),
  })
  const [file, setFile] = useState<OpenFile | null>(null)
  const [conflict, setConflict] = useState<string | null>(null)
  const [allowMissingArmed, setAllowMissingArmed] = useState(false)

  async function selectFile(path: string, content: string, baseHash: string | null) {
    setFile({ path, content, baseHash, dirty: false })
    setConflict(null)
    setAllowMissingArmed(false)
  }

  // Auto-open the primary file once detail lands.
  const detailData = detail.data
  useEffect(() => {
    if (!detailData || file) return
    void (async () => {
      const primary = detailData.standalone ? `${name}.md` : 'SKILL.md'
      const target =
        kind === 'agent'
          ? 'AGENT.md'
          : detailData.files.some((f) => f.path === primary)
            ? primary
            : (detailData.files[0]?.path ?? primary)
      const res = await (kind === 'agent'
        ? api.getAgentFile(name, target)
        : api.getSkillFile(name, target)
      ).catch((e: unknown) => {
        notify(errText(e))
        return null
      })
      if (res) {
        const fh =
          target === (kind === 'agent' ? 'AGENT.md' : primary)
            ? detailData.fileHash
            : (detailData.files.find((f) => f.path === target)?.hash ?? null)
        void selectFile(res.path, res.content, fh)
      }
    })()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detailData, file === null])

  const save = useMutation({
    mutationFn: async (f: OpenFile) => {
      const body = { path: f.path, content: f.content, baseHash: f.baseHash ?? undefined }
      return kind === 'agent'
        ? api.putAgentFile(name, body, allowMissingArmed)
        : api.putSkillFile(name, body)
    },
    onSuccess: (r) => {
      notify(`${t('skills.saved')}: ${r.path} (${r.bytes}B)`)
      setConflict(null)
      setAllowMissingArmed(false)
      if (r.warnings.length > 0) notify(`${t('skills.warnings')}: ${r.warnings.join(' · ')}`)
      void detail.refetch()
      onChanged()
      setFile((prev) => (prev ? { ...prev, dirty: false } : prev))
    },
    onError: (e) => {
      if (e instanceof ApiError && e.status === 409) {
        setConflict(t('skills.conflict409'))
        return
      }
      if (e instanceof ApiError && e.code === 'unknown_tools') {
        setConflict(e.message)
        setAllowMissingArmed(true)
        return
      }
      notify(errText(e))
    },
  })

  async function reread() {
    if (!file) return
    const res = await (kind === 'agent'
      ? api.getAgentFile(name, file.path)
      : api.getSkillFile(name, file.path)
    ).catch((e: unknown) => {
      notify(errText(e))
      return null
    })
    if (!res || !detail.data) return
    const fh =
      file.path === 'AGENT.md' || file.path === 'SKILL.md'
        ? detail.data.fileHash
        : (detail.data.files.find((f) => f.path === file.path)?.hash ?? null)
    void selectFile(res.path, res.content, fh)
    setConflict(null)
  }

  const d: EntryDetail | undefined = detail.data

  return (
    <div className="modal-backdrop" onClick={onClose} style={{ padding: 0, alignItems: 'stretch' }}>
      <div className="drawer" onClick={(ev) => ev.stopPropagation()}>
        <div className="drawer-head">
          <h3 style={{ margin: 0 }}>
            {t('skills.editorTitle', { name, path: file?.path ?? '…' })}
          </h3>
          {file?.dirty ? <span className="dirty-dot">●</span> : null}
          {d ? (
            <span className={`badge prov-badge prov-${d.provenance}`}>{d.provenance}</span>
          ) : null}
          <span className="spacer" />
          <button className="ghost sm" onClick={onClose}>
            ✕
          </button>
        </div>
        <div className="drawer-body">
          {detail.isLoading ? <div className="loading">{t('common.loading')}</div> : null}
          {d ? (
            <>
              <div className="file-list">
                {d.files.map((f) => (
                  <button
                    key={f.path}
                    className={`file-pill${file?.path === f.path ? ' active' : ''}`}
                    onClick={() =>
                      void (async () => {
                        const res = await (kind === 'agent'
                          ? api.getAgentFile(name, f.path)
                          : api.getSkillFile(name, f.path)
                        ).catch((e: unknown) => {
                          notify(errText(e))
                          return null
                        })
                        if (res) void selectFile(res.path, res.content, f.hash)
                      })()
                    }
                  >
                    {f.path} · {(f.size / 1024).toFixed(1)}KB
                  </button>
                ))}
              </div>
              {d.provenance === 'installed' && d.sourceRepo ? (
                <p className="hint">
                  {d.sourceRepo}@{d.commitSha?.slice(0, 8)} · {d.installedAt}
                </p>
              ) : null}
              {file ? (
                <>
                  <textarea
                    value={file.content}
                    onChange={(ev) =>
                      setFile({ ...file, content: ev.target.value, dirty: true })
                    }
                  />
                  {conflict ? (
                    <div className="error-note" style={{ marginTop: 8 }}>
                      {conflict}
                    </div>
                  ) : null}
                  <div className="row" style={{ marginTop: 10 }}>
                    {conflict ? (
                      <button className="ghost sm" onClick={() => void reread()}>
                        {t('skills.reread')}
                      </button>
                    ) : null}
                    <span className="spacer" />
                    <button
                      className="primary"
                      disabled={!file.dirty || save.isPending}
                      onClick={() => save.mutate(file)}
                    >
                      {save.isPending ? t('skills.saving') : t('skills.save')}
                    </button>
                    {allowMissingArmed && conflict ? (
                      <button
                        className="danger sm"
                        disabled={save.isPending}
                        onClick={() => save.mutate(file)}
                      >
                        {t('skills.unknownTools')}
                      </button>
                    ) : null}
                  </div>
                </>
              ) : null}
            </>
          ) : null}
        </div>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Create dialog (skill: name+SKILL.md; agent: structured frontmatter)
// ---------------------------------------------------------------------------

function CreateDialog(props: {
  api: Api
  kind: 'skill' | 'agent'
  onClose: () => void
  onCreated: () => void
  notify: (s: string | null) => void
}) {
  const { api, kind, onClose, onCreated, notify } = props
  const { t } = useTranslation()
  const [name, setName] = useState('')
  const [content, setContent] = useState(
    kind === 'skill'
      ? `---\nname: my-skill\ndescription: What this skill does\n---\n\n# Instructions\n`
      : '',
  )
  const [description, setDescription] = useState('')
  const [model, setModel] = useState('')
  const [tools, setTools] = useState('')
  const [maxIterations, setMaxIterations] = useState('')

  const create = useMutation<CreateEntryResult, Error, void>({
    mutationFn: () => {
      const body = {
        name,
        content,
        ...(kind === 'agent'
          ? {
              description,
              model: model || undefined,
              tools: tools ? tools.split(',').map((s) => s.trim()).filter(Boolean) : undefined,
              maxIterations: maxIterations ? Number(maxIterations) : undefined,
            }
          : {}),
      }
      return kind === 'agent' ? api.createAgent(body) : api.createSkill(body)
    },
    onSuccess: (r) => {
      notify(
        r.warnings.length > 0
          ? `${t('skills.warnings')}: ${r.warnings.join(' · ')}`
          : t('skills.created2'),
      )
      onCreated()
    },
    onError: (e) => notify(errText(e)),
  })

  const ok = name.trim() !== '' && (kind === 'skill' ? content.trim() !== '' : description.trim() !== '')

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" style={{ maxWidth: 620 }} onClick={(ev) => ev.stopPropagation()}>
        <h3>{kind === 'skill' ? t('skills.newSkill') : `+ ${t('nav.agents')}`}</h3>
        <div className="field">
          <label>{t('skills.name')}</label>
          <input
            type="text"
            value={name}
            onChange={(ev) => setName(ev.target.value)}
            placeholder="my-skill"
          />
        </div>
        {kind === 'agent' ? (
          <>
            <div className="field">
              <label>description</label>
              <input
                type="text"
                value={description}
                onChange={(ev) => setDescription(ev.target.value)}
              />
            </div>
            <div className="field">
              <label>model</label>
              <input
                type="text"
                value={model}
                onChange={(ev) => setModel(ev.target.value)}
                placeholder="anthropic/claude-sonnet-4.5"
              />
            </div>
            <div className="field">
              <label>tools (comma-separated)</label>
              <input
                type="text"
                value={tools}
                onChange={(ev) => setTools(ev.target.value)}
                placeholder="read_file, execute_command"
              />
            </div>
            <div className="field">
              <label>max_iterations</label>
              <input
                type="number"
                value={maxIterations}
                onChange={(ev) => setMaxIterations(ev.target.value)}
                placeholder="10"
              />
            </div>
          </>
        ) : null}
        <div className="field">
          <label>{kind === 'skill' ? t('skills.content') : 'instructions (body)'}</label>
          <textarea
            rows={10}
            value={content}
            onChange={(ev) => setContent(ev.target.value)}
            style={{ width: '100%', fontFamily: 'var(--mono)', fontSize: 12.5 }}
          />
        </div>
        <div className="row" style={{ justifyContent: 'flex-end' }}>
          <button className="ghost sm" onClick={onClose}>
            {t('common.cancel')}
          </button>
          <button
            className="primary sm"
            disabled={!ok || create.isPending}
            onClick={() => create.mutate()}
          >
            {t('tasks.create')}
          </button>
        </div>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Install dialog — dry run → acknowledge warnings → install (R2 gate)
// ---------------------------------------------------------------------------

function InstallDialog(props: {
  api: Api
  onClose: () => void
  onInstalled: () => void
  notify: (s: string | null) => void
}) {
  const { api, onClose, onInstalled, notify } = props
  const { t } = useTranslation()
  const [source, setSource] = useState('')
  const [force, setForce] = useState(false)
  const [dry, setDry] = useState<InstallDryRunResult | null>(null)
  const [acked, setAcked] = useState<Set<string>>(new Set())
  const [error, setError] = useState<string | null>(null)

  const dryRun = useMutation<InstallDryRunResult, Error, void>({
    mutationFn: () => api.installSkill({ source, dryRun: true }) as Promise<InstallDryRunResult>,
    onSuccess: (r) => {
      setDry(r)
      setAcked(new Set())
      setError(null)
    },
    onError: (e) => setError(errText(e)),
  })

  const install = useMutation<InstallResult, Error, void>({
    mutationFn: () =>
      api.installSkill({
        source,
        dryRun: false,
        force,
        acknowledgedWarnings: dry ? dry.verdict.warnings.map(warningKey).filter((k) => acked.has(k)) : [],
      }) as Promise<InstallResult>,
    onSuccess: (r) => {
      notify(
        `${t('install.installedOk', { names: r.installed.join(', ') || '—' })}` +
          (r.skipped.length ? ` · ${t('install.skipped', { count: r.skipped.length })}` : ''),
      )
      onInstalled()
      onClose()
    },
    // 409 warnings_unacknowledged → surface, keep dialog open so user can tick more
    onError: (e) => setError(errText(e)),
  })

  const allWarnings = dry?.verdict.warnings ?? []
  const allAcked = allWarnings.length === 0 || allWarnings.every((w) => acked.has(warningKey(w)))
  const canInstall = dry !== null && dry.skills.length > 0 && allAcked && !install.isPending

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(ev) => ev.stopPropagation()}>
        <h3>{t('install.title')}</h3>
        <p className="hint">{t('install.subtitle')}</p>
        <div className="field">
          <label>{t('install.source')}</label>
          <input
            type="text"
            className="mono"
            value={source}
            onChange={(ev) => {
              setSource(ev.target.value)
              setDry(null)
            }}
            placeholder="owner/repo  ·  owner/repo:skills  ·  owner/repo@ref"
          />
        </div>
        <div className="row">
          <label className="hint" style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
            <input type="checkbox" checked={force} onChange={(ev) => setForce(ev.target.checked)} />
            {t('install.force')}
          </label>
          <span className="spacer" />
          <button
            className="ghost sm"
            disabled={!source.trim() || dryRun.isPending}
            onClick={() => dryRun.mutate()}
          >
            {dryRun.isPending ? '…' : t('install.dryRun')}
          </button>
          {dry ? (
            <button className="primary sm" disabled={!canInstall} onClick={() => install.mutate()}>
              {install.isPending ? '…' : t('install.install')}
            </button>
          ) : null}
        </div>

        {error ? <div className="error-note">{error}</div> : null}

        {dry ? (
          <>
            <h4>{t('install.wouldInstall')} ({dry.skills.length})</h4>
            <ul>
              {dry.skills.map((sk) => (
                <li key={sk.name}>
                  <strong>{sk.name}</strong> — {sk.description.slice(0, 80)}{' '}
                  <span className="hint">
                    {sk.files.length} files · {(sk.sizeBytes / 1024).toFixed(1)}KB
                  </span>
                </li>
              ))}
            </ul>
            {dry.verdict.refused.length > 0 ? (
              <>
                <h4 style={{ color: 'var(--red)' }}>{t('install.refused')} ({dry.verdict.refused.length})</h4>
                <ul>
                  {dry.verdict.refused.map((r, i) => (
                    <li key={i}>
                      <span className="mono">{r.skill}/{r.file}</span> [{r.rule}] {r.detail}
                    </li>
                  ))}
                </ul>
              </>
            ) : null}
            {allWarnings.length > 0 ? (
              <>
                <h4>{t('install.warningsNeedAck')} ({allWarnings.length})</h4>
                <div className="row" style={{ marginBottom: 6 }}>
                  <button
                    className="ghost sm"
                    onClick={() => setAcked(new Set(allWarnings.map(warningKey)))}
                  >
                    {t('install.ackAll')}
                  </button>
                </div>
                <ul>
                  {allWarnings.map((w) => {
                    const key = warningKey(w)
                    return (
                      <li key={key}>
                        <label style={{ display: 'flex', gap: 8, alignItems: 'baseline' }}>
                          <input
                            type="checkbox"
                            checked={acked.has(key)}
                            onChange={(ev) => {
                              const next = new Set(acked)
                              if (ev.target.checked) next.add(key)
                              else next.delete(key)
                              setAcked(next)
                            }}
                          />
                          <span>
                            <span className="mono">
                              {w.skill}/{w.file}
                            </span>{' '}
                            [{w.rule}] {w.detail}
                          </span>
                        </label>
                      </li>
                    )
                  })}
                </ul>
              </>
            ) : (
              <p className="hint">{t('install.noWarnings')}</p>
            )}
            {dry.verdict.notes.length > 0 ? (
              <p className="hint">
                {t('install.notes')}: {dry.verdict.notes.join(' · ')}
              </p>
            ) : null}
          </>
        ) : null}

        <div className="row" style={{ justifyContent: 'flex-end', marginTop: 8 }}>
          <button className="ghost sm" onClick={onClose}>
            {t('common.cancel')}
          </button>
        </div>
        <InstalledLedger api={api} />
      </div>
    </div>
  )
}

function InstalledLedger({ api }: { api: Api }) {
  const { t } = useTranslation()
  const ledger = useQuery<InstalledLedgerView>({
    queryKey: ['installed'],
    queryFn: () => api.listInstalled(),
  })
  const rows = [
    ...Object.entries(ledger.data?.skills ?? {}).map(([k, v]) => ({ name: k, kind: 'skill', v })),
    ...Object.entries(ledger.data?.agents ?? {}).map(([k, v]) => ({ name: k, kind: 'agent', v })),
  ]
  return (
    <section className="section">
      <h4>{t('install.ledger')}</h4>
      {rows.length === 0 ? (
        <p className="hint">{t('install.ledgerEmpty')}</p>
      ) : (
        <ul>
          {rows.map((r) => (
            <li key={`${r.kind}:${r.name}`}>
              <strong>{r.name}</strong> <span className="hint">{r.kind}</span> —{' '}
              <span className="mono">
                {r.v.source_repo}@{r.v.commit_sha.slice(0, 8)}
              </span>{' '}
              <span className="hint">{r.v.installed_at}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}
