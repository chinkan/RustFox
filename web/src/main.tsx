import React from 'react'
import ReactDOM from 'react-dom/client'
import { App } from './App'
import { startBootWatcher } from './router'
import './styles.css'

const rootEl = document.getElementById('root')
if (!rootEl) throw new Error('#root element not found')

// Health poll for the restart/reconnect state machine (ADR 0008B).
startBootWatcher()

ReactDOM.createRoot(rootEl).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
)
