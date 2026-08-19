const LOOPBACK_HOSTS = new Set(['127.0.0.1', 'localhost', '::1', '[::1]']);

export function requireOwnedTestBackend(
  raw = process.env.FJ_CONSOLE_BACKEND_URL ?? 'http://127.0.0.1:7700',
  ownershipToken = process.env.FJ_CONSOLE_TEST_INSTANCE_TOKEN
): string {
  if (!ownershipToken || ownershipToken.length < 24) {
    throw new Error(
      'Console browser tests require a runner-owned test-instance token before fixture mutation'
    );
  }
  let backend: URL;
  try {
    backend = new URL(raw);
  } catch {
    throw new Error('Console browser tests require a valid loopback backend URL');
  }

  if (backend.protocol !== 'http:' || !LOOPBACK_HOSTS.has(backend.hostname)) {
    throw new Error(
      `Console browser tests refuse non-loopback or TLS backends: ${backend.origin}`
    );
  }
  return backend.origin;
}
