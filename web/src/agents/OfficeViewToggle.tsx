import { useTranslation } from 'react-i18next'
import type { OfficeVisualize } from './officePrefs'

export function OfficeViewToggle({
  value,
  onChange,
}: {
  value: OfficeVisualize
  onChange: (next: OfficeVisualize) => void
}) {
  const { t } = useTranslation()
  return (
    <div className="office-renderer-toggle" role="group" aria-label={t('agents.officeRendererLabel')}>
      <span className="office-renderer-label">{t('agents.officeRendererLabel')}</span>
      <div className="segmented office-renderer-segmented">
        <button
          type="button"
          className={value === 'svg' ? 'sm primary' : 'sm ghost'}
          aria-pressed={value === 'svg'}
          onClick={() => onChange('svg')}
        >
          {t('agents.officeRendererPixel')}
        </button>
        <button
          type="button"
          className={value === 'webgl' ? 'sm primary' : 'sm ghost'}
          aria-pressed={value === 'webgl'}
          onClick={() => onChange('webgl')}
        >
          {t('agents.officeRenderer3d')}
        </button>
      </div>
    </div>
  )
}
