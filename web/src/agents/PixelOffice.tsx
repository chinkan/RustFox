import { useTranslation } from 'react-i18next'
import type { Agent } from '../api/types'
import { FoxSprite, type FoxFacing } from './FoxSprite'
import { OfficeDecor } from './officeSprites'
import { foxVariant, roamDelaySeconds, seatView, shouldRoam, visibleDeskSlice } from './seats'

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
  const top = visible.filter((_, i) => i % 2 === 0)
  const bottom = visible.filter((_, i) => i % 2 === 1)

  return (
    <div
      className="agent-office"
      role="region"
      aria-label={t('agents.officeLabel')}
      data-motion={motion ? 'on' : 'off'}
    >
      <p className="agent-office-hint">{t('agents.officeHint')}</p>
      <div className="office-room" data-decor="office">
        <OfficeDecor />
        {visible.length === 0 ? (
          <div className="empty">
            <div className="big">🔍</div>
            <p>{t('agents.noneFound')}</p>
          </div>
        ) : (
          <div className="office-scene">
            <div className="office-row" data-row="top">
              {top.map((agent) => (
                <DeskCard key={agent.id} agent={agent} motion={motion} facing="down" />
              ))}
            </div>
            <div className="office-aisle" data-aisle="roam">
              <span className="office-plant" aria-hidden="true" />
              {visible.map((agent) => {
                const view = seatView(agent)
                if (!shouldRoam(view, motion)) return null
                const variant = foxVariant(agent.name)
                return (
                  <div
                    key={agent.id}
                    className="office-roamer"
                    data-roam="on"
                    data-lane={roamDelaySeconds(agent.name) > 3.5 ? '1' : '0'}
                    style={{ animationDelay: `-${roamDelaySeconds(agent.name)}s` }}
                  >
                    <FoxSprite
                      pose="idle"
                      variant={variant}
                      mode="walk"
                      facing={roamDelaySeconds(agent.name) > 3.5 ? 'left' : 'right'}
                    />
                  </div>
                )
              })}
            </div>
            <div className="office-row" data-row="bottom">
              {bottom.map((agent) => (
                <DeskCard key={agent.id} agent={agent} motion={motion} facing="up" />
              ))}
              {overflow > 0 ? (
                <button type="button" className="agent-desk-more" onClick={onShowOverflow}>
                  {t('agents.moreDesks', { count: overflow })}
                </button>
              ) : null}
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

function DeskCard({
  agent,
  motion,
  facing,
}: {
  agent: Agent
  motion: boolean
  facing: FoxFacing
}) {
  const { t } = useTranslation()
  const view = seatView(agent)
  const variant = foxVariant(agent.name)
  const roaming = shouldRoam(view, motion)
  const returning = motion && view.pose === 'running'
  const statusLabel = t(`agents.${view.pose}`)
  const nameBits = [agent.name, statusLabel]
  if (view.definitionOnly) {
    nameBits.push(t('agents.definitionOnly'), t('agents.noLiveRun'))
  }
  return (
    <article
      className="agent-desk"
      data-pose={view.pose}
      data-definition-only={view.definitionOnly ? 'true' : 'false'}
      data-roaming={roaming ? 'true' : 'false'}
      data-returning={returning ? 'true' : 'false'}
      data-coat={variant.coat}
      data-accessory={variant.accessory}
      data-desk-prop={variant.deskProp}
      data-facing={facing}
      data-dir={facing}
      aria-label={nameBits.join(', ')}
    >
      <div className={returning ? 'fox-arriving' : undefined}>
        <FoxSprite pose={view.pose} variant={variant} mode={roaming ? 'desk' : 'seat'} facing={facing} />
      </div>
      <strong className="agent-desk-name">{agent.name}</strong>
      <span className="agent-desk-model mono">{agent.model}</span>
      <SeatStatus agent={agent} />
    </article>
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
