import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'

export function GoogleConnect({
  api,
}: {
  api: {
    googleStatus: () => Promise<{ offered: boolean }>
    startGoogle: () => Promise<{ offered: boolean; authorizeUrl?: string }>
    setGoogleClientId: (clientId: string) => Promise<{ offered: boolean }>
  }
}) {
  const queryClient = useQueryClient()
  const google = useQuery({ queryKey: ['google-connector'], queryFn: api.googleStatus })
  const [clientId, setClientId] = useState('')
  const saveId = useMutation({
    mutationFn: () => api.setGoogleClientId(clientId),
    onSuccess: () => {
      setClientId('')
      void queryClient.invalidateQueries({ queryKey: ['google-connector'] })
    },
  })
  return (
    <section className="section">
      <h2 className="section-title">Google</h2>
      <div className="card">
        {google.data?.offered ? (
          <button
            type="button"
            className="primary"
            onClick={() => {
              void api.startGoogle().then((res) => {
                if (res.offered && res.authorizeUrl) window.location.href = res.authorizeUrl
              })
            }}
          >
            Google
          </button>
        ) : null}
        <details style={{ marginTop: 12 }}>
          <summary>Advanced</summary>
          <div className="field">
            <label>Desktop client id</label>
            <input
              type="password"
              autoComplete="off"
              value={clientId}
              onChange={(e) => setClientId(e.target.value)}
            />
          </div>
          <button type="button" className="btn-secondary" onClick={() => saveId.mutate()} disabled={saveId.isPending}>
            Save
          </button>
        </details>
      </div>
    </section>
  )
}
