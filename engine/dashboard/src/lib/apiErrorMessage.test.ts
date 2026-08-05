import { describe, expect, it } from 'vitest';
import { extractApiErrorMessage } from './apiErrorMessage';

describe('extractApiErrorMessage', () => {
  it('prefers nonempty string response bodies over transport fallback text', () => {
    expect(
      extractApiErrorMessage(
        {
          response: { data: 'backend string failure' },
          message: 'ignored transport failure',
        },
        'default failure',
      ),
    ).toBe('backend string failure');
  });

  it('prefers nonempty message fields from object response bodies', () => {
    expect(
      extractApiErrorMessage(
        {
          response: { data: { message: 'backend object failure' } },
          message: 'ignored transport failure',
        },
        'default failure',
      ),
    ).toBe('backend object failure');
  });

  it('uses transport error text when the response body has no message', () => {
    expect(
      extractApiErrorMessage(
        {
          response: { data: { status: 'error' } },
          message: 'network request failed',
        },
        'default failure',
      ),
    ).toBe('network request failed');
  });

  it('uses the supplied fallback when neither response nor transport text is available', () => {
    expect(extractApiErrorMessage({}, 'default failure')).toBe('default failure');
  });
});
