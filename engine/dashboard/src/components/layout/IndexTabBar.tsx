import { useEffect, useRef } from 'react'
import { NavLink, useLocation } from 'react-router-dom'
import { useSettings } from '@/hooks/useSettings'
import { useHealthDetail } from '@/hooks/useSystemStatus'
import { cn } from '@/lib/utils'
import { buildIndexTabHref, getVisibleIndexTabs } from './index-tab-contract'

interface IndexTabBarProps {
  indexName: string
}

export function IndexTabBar({ indexName }: IndexTabBarProps) {
  const { data: settings } = useSettings(indexName)
  const { data: health } = useHealthDetail()
  const location = useLocation()
  const tabScrollContainerRef = useRef<HTMLDivElement | null>(null)

  useEffect(() => {
    const activeTab = tabScrollContainerRef.current?.querySelector<HTMLAnchorElement>('a[aria-current="page"]')
    if (activeTab && typeof activeTab.scrollIntoView === 'function') {
      activeTab.scrollIntoView({
        block: 'nearest',
        inline: 'nearest',
      })
    }
  }, [location.pathname])

  const visibleTabs = getVisibleIndexTabs(settings, health?.capabilities.vectorSearch)

  return (
    <section className="mb-6 border-b border-border" data-testid="index-tab-bar">
      <div
        ref={tabScrollContainerRef}
        data-testid="index-tab-bar-scroll"
        className="overflow-x-auto [-webkit-overflow-scrolling:touch]"
      >
        <nav aria-label="Index sections">
          <ul
            data-testid="index-tab-bar-list"
            className="flex min-w-max items-center gap-1 whitespace-nowrap py-1"
          >
            {visibleTabs.map((tabDefinition) => {
              const href = buildIndexTabHref(indexName, tabDefinition.relativePath)
              return (
                <li key={tabDefinition.id}>
                  <NavLink
                    to={href}
                    end={tabDefinition.end}
                    data-testid={`index-tab-${tabDefinition.id}`}
                    className={({ isActive }) =>
                      cn(
                        'inline-flex max-w-[11rem] items-center overflow-hidden text-ellipsis rounded-md px-3 py-2 text-sm font-medium transition-colors',
                        isActive
                          ? 'bg-background text-foreground shadow-sm'
                          : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground',
                      )
                    }
                  >
                    {tabDefinition.label}
                  </NavLink>
                </li>
              )
            })}
          </ul>
        </nav>
      </div>
    </section>
  )
}
