import { createRootRouteWithContext, Link, Outlet } from '@tanstack/react-router'
import type { RouterContext } from '../auth'

export const Route = createRootRouteWithContext<RouterContext>()({
  component: RootComponent,
  notFoundComponent: NotFound,
})

function RootComponent() {
  return <Outlet />
}

function NotFound() {
  return (
    <div className="login-wrap">
      <div className="login-card">
        <h1>404 — Route not found</h1>
        <p>
          TanStack Router matched no route for this URL. This page is
          type-checked at build time, so a broken link is usually a compile
          error rather than a runtime surprise.
        </p>
        <Link to="/dashboard" className="btn primary">
          Back to dashboard
        </Link>
      </div>
    </div>
  )
}
