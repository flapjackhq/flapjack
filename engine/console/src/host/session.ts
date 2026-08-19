import type { ConsoleTransport } from '../lib/transport/console_transport';

export interface AuthenticatedSession {
  transport: ConsoleTransport;
  signOut(): Promise<void>;
}

export interface SessionProvider {
  restore(): Promise<AuthenticatedSession | null>;
  signIn(apiKey: string): Promise<AuthenticatedSession>;
}
