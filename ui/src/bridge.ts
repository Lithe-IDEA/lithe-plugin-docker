// All engine access stays in the backend. The page uses only the host bridge.
interface Lithe {
  version: number;
  context: { platform: string; locale: string; theme: string };
  request(method: string, params?: object): Promise<unknown>;
  on(event: string, listener: (value: Progress) => void): () => void;
}
interface Progress { call: string; percentage?: number }
declare global { interface Window { lithe?: Lithe } }

export const lithe = window.lithe;
let sequence = 0;
const prefix = crypto.randomUUID();
export interface Call<T> { result: Promise<T>; cancel(): void }

export function invoke<T>(fn: string, params: object = {}, signal?: AbortSignal,
  progress?: (percentage?: number) => void): Call<T> {
  const call = `${prefix}-${sequence++}`;
  const cancel = () => { void lithe?.request('functions.cancel', { call }).catch(() => {}); };
  const unsubscribe = progress && lithe?.on('functions.progress', event => {
    if (event.call === call) progress(event.percentage);
  });
  const result = (async () => {
    if (signal?.aborted) throw new DOMException('Cancelled', 'AbortError');
    if (!lithe) throw { code: 'plugin.web.unavailable' };
    signal?.addEventListener('abort', cancel, { once: true });
    try {
      const answer = await lithe.request('functions.invoke', { function: fn, params, call }) as { value: T };
      if (signal?.aborted) throw new DOMException('Cancelled', 'AbortError');
      return answer.value;
    } finally {
      signal?.removeEventListener('abort', cancel);
      unsubscribe?.();
    }
  })();
  return { result, cancel };
}

export function cancelled(error: unknown): boolean {
  const code = (error as { code?: string })?.code;
  return (error instanceof DOMException && error.name === 'AbortError') || code === 'requestCancelled';
}

export async function restoreState<T>(): Promise<T | null> {
  if (!lithe) return null;
  const answer = await lithe.request('ui.getState') as { state: T | null };
  return answer.state;
}
export function saveState(state: object): void {
  void lithe?.request('ui.setState', { state }).catch(() => {});
}
