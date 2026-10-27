import { useTranslation } from 'react-i18next'
import type { Agent } from '../api/types'
import { FoxSprite } from './FoxSprite'
import { seatView, visibleDeskSlice } from './seats'

export function PixelOffice({
  agents,
  motion,
  onShowOverflow,
}: {
  agents: Agent[]
  motion: boolean
  onShowOverflow: () => void
}) {
  const { t } = useTranslation()
  const { visible, overflow } = visibleDeskSlice(agents)

  return (
    <div
      className="agent-office"
      role="region"
      aria-label={t('agents.officeLabel')}
      data-motion={motion ? 'on' : 'off'}
    >
      <p className="agent-office-hint">{t('agents.officeHint')}</p>
      {visible.length === 0 ? (
        <div className="empty">
          <div className="big">🔍</div>
          <p>{t('agents.noneFound')}</p>
        </div>
      ) : (
        <div className="agent-office-floor">
          {visible.map((agent) => {
            const view = seatView(agent)
            const statusLabel = t(`agents.${view.pose}`)
            const nameBits = [agent.name, statusLabel]
            if (view.definitionOnly) {
              nameBits.push(t('agents.definitionOnly'), t('agents.noLiveRun'))
            }
            return (
              <article
                key={agent.id}
                className="agent-desk"
                data-pose={view.pose}
                data-definition-only={view.definitionOnly ? 'true' : 'false'}
                aria-label={nameBits.join(', ')}
              >
                <FoxSprite pose={view.pose} />
                <strong className="agent-desk-name">{agent.name}</strong>
                <span className="agent-desk-model mono">{agent.model}</span>
                <SeatStatus agent={agent} />
              </article>
            )
          })}
          {overflow > 0 ? (
            <button type="button" className="agent-desk-more" onClick={onShowOverflow}>
              {t('agents.moreDesks', { count: overflow })}
            </button>
          ) : null}
        </div>
      )}
    </div>
  )
}

export function SeatStatus({ agent }: { agent: Agent }) {
  const { t } = useTranslation()
  const view = seatView(agent)
  return (
    <span className="seat-status">
      <span className={`badge dot ${view.pose}`}>{t(`agents.${view.pose}`)}</span>
      {view.definitionOnly ? (
        <>
          <span className="badge">{t('agents.definitionOnly')}</span>
          <span className="seat-nolive">{t('agents.noLiveRun')}</span>
        </>
      ) : null}
    </span>
  )
}
