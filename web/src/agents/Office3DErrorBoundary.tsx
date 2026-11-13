import { Component, type ErrorInfo, type ReactNode } from 'react'

/**
 * Catches R3F / Three runtime errors so the route ErrorBoundary never owns Office view.
 * Parent should flip to Pixel via `onError` (settings may still say webgl).
 */
export class Office3DErrorBoundary extends Component<
  {
    children: ReactNode
    fallback: ReactNode
    onError: (error: Error) => void
  },
  { failed: boolean }
> {
  state = { failed: false }

  static getDerivedStateFromError(): { failed: boolean } {
    return { failed: true }
  }

  componentDidCatch(error: Error, _info: ErrorInfo) {
    this.props.onError(error)
  }

  render() {
    if (this.state.failed) return this.props.fallback
    return this.props.children
  }
}
