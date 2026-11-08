import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import App from './App.jsx'
import { runtime } from './lib/runtime'

// Lets the stylesheet adapt to each platform's window chrome.
document.documentElement.dataset.platform = runtime.platform

createRoot(document.getElementById('root')).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
