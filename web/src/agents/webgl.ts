export interface WebGLProbe {
  ok: boolean
  reason?: string
}

/** Lightweight WebGL availability check (no React Three mount required). */
export function probeWebGL(): WebGLProbe {
  if (typeof document === 'undefined') {
    return { ok: false, reason: 'no-document' }
  }
  try {
    const canvas = document.createElement('canvas')
    const gl =
      canvas.getContext('webgl2', { failIfMajorPerformanceCaveat: false }) ??
      canvas.getContext('webgl', { failIfMajorPerformanceCaveat: false }) ??
      canvas.getContext('experimental-webgl')
    if (!gl) return { ok: false, reason: 'no-context' }
    return { ok: true }
  } catch {
    return { ok: false, reason: 'exception' }
  }
}
