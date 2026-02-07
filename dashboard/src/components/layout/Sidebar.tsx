import { NavLink, useLocation } from 'react-router-dom';
import { Home, Settings as SettingsIcon, Key, Activity, X } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useEffect } from 'react';

interface SidebarProps {
  open?: boolean;
  onClose?: () => void;
}

const navItems = [
  { to: '/overview', icon: Home, label: 'Overview' },
  { to: '/settings', icon: SettingsIcon, label: 'Settings' },
  { to: '/keys', icon: Key, label: 'API Keys' },
  { to: '/system', icon: Activity, label: 'System' },
];

export function Sidebar({ open, onClose }: SidebarProps) {
  const location = useLocation();

  // Close sidebar on route change (mobile)
  useEffect(() => {
    onClose?.();
  }, [location.pathname]); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <>
      {/* Mobile overlay */}
      {open && (
        <div
          className="fixed inset-0 z-40 bg-black/50 md:hidden"
          onClick={onClose}
        />
      )}

      {/* Sidebar */}
      <aside
        className={cn(
          'border-r border-border bg-background p-4 z-50',
          // Desktop: always visible, static
          'hidden md:block w-64',
          // Mobile: slide-in overlay
          open && 'fixed inset-y-0 left-0 block w-64 md:relative md:inset-auto'
        )}
      >
        {/* Mobile close button */}
        <div className="flex items-center justify-between mb-4 md:hidden">
          <span className="text-sm font-semibold text-muted-foreground">Navigation</span>
          <button
            onClick={onClose}
            className="p-1 rounded-md hover:bg-accent"
            aria-label="Close navigation"
          >
            <X className="h-5 w-5" />
          </button>
        </div>

        <nav className="space-y-2">
          {navItems.map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              className={({ isActive }) =>
                cn(
                  'flex items-center gap-3 px-4 py-2 rounded-md text-sm font-medium transition-colors',
                  isActive
                    ? 'bg-primary text-primary-foreground'
                    : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground'
                )
              }
            >
              <item.icon className="h-5 w-5" />
              {item.label}
            </NavLink>
          ))}
        </nav>
      </aside>
    </>
  );
}
