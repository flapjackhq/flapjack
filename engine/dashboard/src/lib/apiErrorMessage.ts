export type ApiErrorLike = {
  response?: {
    data?: unknown;
  };
  message?: unknown;
};

export function extractApiErrorMessage(error: unknown, fallbackMessage: string): string {
  const responseData = (error as ApiErrorLike)?.response?.data;

  if (typeof responseData === 'string' && responseData.length > 0) {
    return responseData;
  }

  if (responseData && typeof responseData === 'object') {
    const message = (responseData as { message?: unknown }).message;
    if (typeof message === 'string' && message.length > 0) {
      return message;
    }
  }

  const transportMessage = (error as ApiErrorLike)?.message;
  if (typeof transportMessage === 'string' && transportMessage.length > 0) {
    return transportMessage;
  }

  return fallbackMessage;
}
