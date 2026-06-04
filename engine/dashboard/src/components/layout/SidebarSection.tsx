import type { ReactNode } from 'react';
import { NavLink, useLocation } from 'react-router-dom';
import { cn } from '@/lib/utils';
import type { SidebarNavItem, SidebarSectionId } from './sidebar-nav';

interface SidebarSectionProps {
  sectionId: SidebarSectionId;
  heading: ReactNode;
  headingLabel: string;
  items: SidebarNavItem[];
  headingTestId?: string;
  headingSuffix?: ReactNode;
  sectionTestId?: string;
  children?: ReactNode;
}

const sectionLinkClassName =
  'flex items-center gap-3 px-4 py-2 rounded-md text-sm font-medium transition-colors';

export function SidebarSection({
  sectionId,
  heading,
  headingLabel,
  items,
  headingTestId,
  headingSuffix,
  sectionTestId,
  children,
}: SidebarSectionProps) {
  const location = useLocation();

  return (
    <section className="mt-6 first:mt-0" data-testid={sectionTestId}>
      <div
        className="px-4 mb-2 text-xs font-semibold text-muted-foreground uppercase tracking-wider flex items-center gap-1.5"
        data-testid={headingTestId}
      >
        {heading}
        {headingSuffix}
      </div>
      <nav
        className="space-y-2"
        data-testid={`sidebar-section-links-${sectionId}`}
        aria-label={headingLabel}
      >
        {items.map((item) => (
          <NavLink
            key={item.to}
            to={item.to}
            className={({ isActive }) =>
              cn(
                sectionLinkClassName,
                isActive || item.activePaths?.includes(location.pathname)
                  ? 'bg-primary/15 text-primary font-semibold'
                  : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground',
              )
            }
          >
            <item.icon className="h-5 w-5" />
            {item.label}
          </NavLink>
        ))}
      </nav>
      {children}
    </section>
  );
}
