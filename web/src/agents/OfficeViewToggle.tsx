import { useTranslation } from 'react-i18next'
import type { OfficeRenderer } from './officeViewMode'

export function OfficeViewToggle({
  value,
  onChange,
}: {
  value: OfficeRenderer
  onChange: (next: OfficeRenderer) => void
}) {
  const { t } = useTranslation()
  return (
    <div className="office-renderer-toggle" role="group" aria-label={t('agents.officeRendererLabel')}>
      <span className="office-renderer-label">{t('agents.officeRendererLabel')}</span>
      <div className="segmented office-renderer-segmented">
        <button
          type="button"
          className={value === 'pixel' ? 'sm primary' : 'sm ghost'}
          aria-pressed={value === 'pixel'}
          onClick={() => onChange('pixel')}
        >
          {t('agents.officeRendererPixel')}
        </button>
        <button
          type="button"
          className={value === '3d' ? 'sm primary' : 'sm ghost'}
          aria-pressed={value === '3d'}
          onClick={() => onChange('3d')}
        >
          {t('agents.officeRenderer3d')}
        </button>
      </div>
    </div>
  )
}
