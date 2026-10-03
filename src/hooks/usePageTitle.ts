import { useEffect } from 'react'
import { useLocation } from 'react-router'

const PAGE_TITLES: Record<string, string> = {
  '/': 'Home',
  '/faq': 'FAQ',
  '/setup': 'Setup',
  '/login': 'Login',
  '/reset-password': 'Reset Password',
  '/download': 'Download',
  '/broker': 'Select Broker',
  '/dashboard': 'Dashboard',
  '/positions': 'Positions',
  '/orderbook': 'Order Book',
  '/tradebook': 'Trade Book',
  '/holdings': 'Holdings',
  '/search': 'Search',
  '/search/token': 'Token Search',
  '/apikey': 'API Key',
  '/platforms': 'Platforms',
  '/tradingview': 'TradingView',
  '/gocharting': 'GoCharting',
  '/pnl-tracker': 'P&L Tracker',
  '/sandbox': 'Sandbox',
  '/sandbox/mypnl': 'Sandbox P&L',
  '/analyzer': 'Analyzer',
  '/tools': 'Tools',
  '/strategybuilder': 'Strategy Builder',
  '/strategybuilder/portfolio': 'Strategy Portfolio',
  '/optionchain': 'Option Chain',
  '/ivchart': 'IV Chart',
  '/oitracker': 'OI Tracker',
  '/oirange': 'OI Range',
  '/gammadensity': 'Gamma Density',
  '/maxpain': 'Max Pain',
  '/straddle': 'Straddle Chart',
  '/straddlepnl': 'Straddle P&L',
  '/volsurface': 'Vol Surface',
  '/gex': 'GEX Dashboard',
  '/ivsmile': 'IV Smile',
  '/oiprofile': 'OI Profile',
  '/websocket/test': 'WebSocket Test',
  '/websocket/order': 'Order Stream',
  // Desktop: /python and /flow titles removed (out of scope).
  '/strategy': 'Strategies',
  '/strategy/new': 'New Strategy',
  '/chartink': 'Chartink Strategies',
  '/chartink/new': 'New Chartink Strategy',
  '/leverage': 'Leverage',
  '/admin': 'Admin',
  '/admin/freeze': 'Freeze Qty',
  '/admin/holidays': 'Holidays',
  '/admin/timings': 'Market Timings',
  '/telegram': 'Telegram',
  '/telegram/config': 'Telegram Config',
  '/telegram/users': 'Telegram Users',
  '/telegram/analytics': 'Telegram Analytics',
  '/logs': 'Logs',
  '/logs/live': 'Live Logs',
  '/logs/sandbox': 'Sandbox Logs',
  '/logs/security': 'Security',
  '/logs/traffic': 'Traffic',
  '/logs/latency': 'Latency',
  '/health': 'Health Monitor',
  '/profile': 'Profile',
  '/master-contract': 'Master Contract',
  '/action-center': 'Action Center',
  '/playground': 'Playground',
  '/historify': 'Historify',
  '/historify/charts': 'Historify Charts',
}

/** Dynamic route patterns for parameterized routes */
const DYNAMIC_TITLES: Array<{ pattern: RegExp; title: string }> = [
  // Desktop: /python and /flow patterns removed (out of scope).
  { pattern: /^\/strategy\/[^/]+\/edit$/, title: 'Edit Strategy' },
  { pattern: /^\/strategy\/[^/]+$/, title: 'Strategy' },
  { pattern: /^\/chartink\/[^/]+\/configure$/, title: 'Configure Chartink' },
  { pattern: /^\/chartink\/[^/]+$/, title: 'View Chartink Strategy' },
  { pattern: /^\/historify\/charts\/[^/]+$/, title: 'Historify Charts' },
  { pattern: /^\/websocket\/test\/\d+$/, title: 'WebSocket Test' },
]

function getPageTitle(pathname: string): string {
  if (PAGE_TITLES[pathname]) {
    return PAGE_TITLES[pathname]
  }

  for (const { pattern, title } of DYNAMIC_TITLES) {
    if (pattern.test(pathname)) {
      return title
    }
  }

  return 'OpenAlgo'
}

export function usePageTitle() {
  const { pathname } = useLocation()

  useEffect(() => {
    const title = getPageTitle(pathname)
    document.title = title === 'OpenAlgo' ? 'OpenAlgo' : `${title} | OpenAlgo`
  }, [pathname])
}
