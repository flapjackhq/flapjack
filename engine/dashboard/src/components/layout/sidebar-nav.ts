import type { LucideIcon } from 'lucide-react';
import {
  Activity,
  ArrowRightLeft,
  Bug,
  Database,
  Home,
  Key,
  Lightbulb,
  BarChart3,
  ScrollText,
  ShieldCheck,
  SlidersHorizontal,
} from 'lucide-react';

export type SidebarSectionId = 'indexes' | 'intelligence' | 'developer' | 'system';

export interface SidebarNavItem {
  to: string;
  label: string;
  icon: LucideIcon;
  activePaths?: readonly string[];
}

export interface SidebarSectionDefinition {
  id: SidebarSectionId;
  heading: string;
  items: SidebarNavItem[];
}

export const SIDEBAR_SECTION_DEFINITIONS: SidebarSectionDefinition[] = [
  {
    id: 'indexes',
    heading: 'Indexes',
    items: [
      { to: '/overview', icon: Home, label: 'Overview', activePaths: ['/'] },
    ],
  },
  {
    id: 'intelligence',
    heading: 'Intelligence',
    items: [
      { to: '/query-suggestions', icon: Lightbulb, label: 'Query Suggestions' },
      { to: '/experiments', icon: Database, label: 'Experiments' },
      { to: '/personalization', icon: SlidersHorizontal, label: 'Personalization' },
    ],
  },
  {
    id: 'developer',
    heading: 'Developer',
    items: [
      { to: '/keys', icon: Key, label: 'API Keys' },
      { to: '/security-sources', icon: ShieldCheck, label: 'Security Sources' },
      { to: '/dictionaries', icon: ScrollText, label: 'Dictionaries' },
      { to: '/logs', icon: ScrollText, label: 'API Logs' },
      { to: '/events', icon: Bug, label: 'Event Debugger' },
    ],
  },
  {
    id: 'system',
    heading: 'System',
    items: [
      { to: '/migrate', icon: ArrowRightLeft, label: 'Migrate' },
      { to: '/metrics', icon: BarChart3, label: 'Metrics' },
      { to: '/cluster', icon: Database, label: 'Cluster' },
      { to: '/system', icon: Activity, label: 'System' },
    ],
  },
];
