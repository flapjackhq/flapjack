/**
 * @module Authentication gate and login screen that protect the Flapjack dashboard behind Admin API key validation.
 */
import { useState, useCallback, useEffect } from 'react';
import { useAuth } from '@/hooks/useAuth';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { KeyRound, AlertCircle, CheckCircle2, Loader2 } from 'lucide-react';

type ValidationState = 'idle' | 'validating' | 'valid' | 'invalid';

/**
 * Guards the application behind API key authentication.
 * 
 * On mount, validates any stored API key against the server. If the key is missing or stale (403), clears it and shows the login screen. While validation is in progress, renders a loading spinner.
 * 
 * @param children - Application content to render once authenticated
 */
export function AuthGate({ children }: { children: React.ReactNode }) {
  const { setSessionAuthenticated } = useAuth();
  const [needsAuth, setNeedsAuth] = useState<boolean | null>(null);

  // On mount, validate the HttpOnly session (or check if auth is required).
  useEffect(() => {
    const headers: Record<string, string> = {
      'x-algolia-application-id': 'flapjack',
      'Content-Type': 'application/json',
    };
    fetch('/1/indexes', { headers })
      .then((res) => {
        if (res.ok) {
          setSessionAuthenticated(true);
          setNeedsAuth(false);
        } else if (res.status === 403) {
          setSessionAuthenticated(false);
          setNeedsAuth(true);
        }
      })
      .catch(() => {
        // Server unreachable — show auth gate since we can't tell
        setNeedsAuth(true);
      });
  }, [setSessionAuthenticated]);

  // Still checking
  if (needsAuth === null) {
    return (
      <div className="flex h-screen items-center justify-center bg-background">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (needsAuth) {
    return <AuthScreen onAuthenticated={() => setNeedsAuth(false)} />;
  }

  return <>{children}</>;
}

/**
 * Login screen for entering and validating an Admin API key.
 * 
 * Submits the key to the server's indexes endpoint for validation. On success, persists the key via the auth store after a brief visual confirmation delay. Displays contextual help for locating or resetting the key.
 */
function AuthScreen({ onAuthenticated }: { onAuthenticated: () => void }) {
  const { login } = useAuth();
  const [keyInput, setKeyInput] = useState('');
  const [validation, setValidation] = useState<ValidationState>('idle');
  const [errorMessage, setErrorMessage] = useState('');

  const handleSubmit = useCallback(
    async (e: React.FormEvent) => {
      e.preventDefault();
      const key = keyInput.trim();
      if (!key) return;

      setValidation('validating');
      setErrorMessage('');

      try {
        if (await login(key)) {
          setValidation('valid');
          setTimeout(onAuthenticated, 400);
        } else {
          setValidation('invalid');
          setErrorMessage('Invalid API key. Check your terminal for the correct key.');
        }
      } catch {
        setValidation('invalid');
        setErrorMessage('Could not connect to server.');
      }
    },
    [keyInput, login, onAuthenticated]
  );

  return (
    <div className="flex min-h-screen items-center justify-center bg-background p-4" data-testid="auth-gate">
      <Card className="w-full max-w-md shadow-lg">
        <CardHeader className="text-center pb-2">
          <div className="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-full bg-primary/10">
            <span className="text-3xl">🥞</span>
          </div>
          <CardTitle className="text-2xl">Welcome to Flapjack</CardTitle>
          <p className="text-sm text-muted-foreground mt-2">
            Enter your Admin API Key to access the dashboard.
          </p>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSubmit} className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="api-key" className="flex items-center gap-2">
                <KeyRound className="h-4 w-4" />
                Admin API Key
              </Label>
              <Input
                id="api-key"
                type="password"
                value={keyInput}
                onChange={(e) => {
                  setKeyInput(e.target.value);
                  if (validation === 'invalid') {
                    setValidation('idle');
                    setErrorMessage('');
                  }
                }}
                placeholder="fj_..."
                autoFocus
                data-testid="auth-key-input"
                className={
                  validation === 'invalid'
                    ? 'border-red-500 focus-visible:ring-red-500'
                    : validation === 'valid'
                    ? 'border-green-500 focus-visible:ring-green-500'
                    : ''
                }
              />
            </div>

            {validation === 'invalid' && errorMessage && (
              <div className="flex items-center gap-2 text-sm text-red-600 dark:text-red-400" data-testid="auth-error">
                <AlertCircle className="h-4 w-4 shrink-0" />
                {errorMessage}
              </div>
            )}

            {validation === 'valid' && (
              <div className="flex items-center gap-2 text-sm text-green-600 dark:text-green-400" data-testid="auth-success">
                <CheckCircle2 className="h-4 w-4 shrink-0" />
                Authenticated! Loading dashboard...
              </div>
            )}

            <Button
              type="submit"
              className="w-full"
              disabled={!keyInput.trim() || validation === 'validating' || validation === 'valid'}
              data-testid="auth-submit"
            >
              {validation === 'validating' ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Validating...
                </>
              ) : validation === 'valid' ? (
                <>
                  <CheckCircle2 className="mr-2 h-4 w-4" />
                  Connected
                </>
              ) : (
                'Connect'
              )}
            </Button>
          </form>

          <div className="mt-6 rounded-md bg-muted/50 p-3 text-xs text-muted-foreground space-y-1.5" data-testid="auth-help">
            <p className="font-medium text-foreground">Where to find your API key:</p>
            <p>Check the terminal where Flapjack is running. The key is displayed on startup next to the 🔑 icon.</p>
            <p>
              Lost your key? Run:{' '}
              <code className="rounded bg-muted px-1 py-0.5 font-mono text-foreground">
                flapjack --data-dir &lt;data-dir&gt; reset-admin-key
              </code>
            </p>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
