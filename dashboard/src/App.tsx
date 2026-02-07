import { Routes, Route } from 'react-router-dom';
import { useTheme } from './hooks/useTheme';
import { Layout } from './components/layout/Layout';
import { ErrorBoundary } from './components/ErrorBoundary';
import { Overview } from './pages/Overview';
import { SearchBrowse } from './pages/SearchBrowse';
import { Settings } from './pages/Settings';
import { ApiKeys } from './pages/ApiKeys';
import { System } from './pages/System';
import { Toaster } from './components/ui/toaster';

function App() {
  // Initialize theme
  useTheme();

  return (
    <>
      <Routes>
        <Route path="/" element={<Layout />}>
          <Route index element={<ErrorBoundary><Overview /></ErrorBoundary>} />
          <Route path="overview" element={<ErrorBoundary><Overview /></ErrorBoundary>} />
          <Route path="search/:indexName" element={<ErrorBoundary><SearchBrowse /></ErrorBoundary>} />
          <Route path="settings" element={<ErrorBoundary><Settings /></ErrorBoundary>} />
          <Route path="keys" element={<ErrorBoundary><ApiKeys /></ErrorBoundary>} />
          <Route path="system" element={<ErrorBoundary><System /></ErrorBoundary>} />
          <Route path="*" element={<div className="p-6">Page not found</div>} />
        </Route>
      </Routes>
      <Toaster />
    </>
  );
}

export default App;
