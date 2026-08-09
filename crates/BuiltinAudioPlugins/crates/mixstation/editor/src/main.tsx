import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { HashRouter, Route, Routes } from 'react-router-dom'
import App from './App'
import './styles.css'

// HashRouter, not BrowserRouter: the page is served from the `mikoplugin:`
// custom scheme, where the History API has no real document URL to push against.
// The hash is the only navigation the CEF host can round-trip reliably.
createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <HashRouter>
      <Routes>
        <Route path="/instance/:instanceId" element={<App />} />
        <Route path="*" element={<App />} />
      </Routes>
    </HashRouter>
  </StrictMode>,
)
