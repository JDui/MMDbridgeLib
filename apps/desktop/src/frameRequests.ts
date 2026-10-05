export type FrameRequestQueue = { request: (frame: number) => Promise<void>; dispose: () => void; readonly pending: boolean };

export function createFrameRequestQueue<T>(load: (frame: number) => Promise<T>, apply: (value: T) => void, reportError: (reason: unknown) => void): FrameRequestQueue {
  let active = true;
  let pending = false;
  let wanted: number | null = null;
  return {
    get pending() { return pending; },
    dispose() { active = false; wanted = null; },
    async request(frame) {
      if (!active) return;
      wanted = frame;
      if (pending) return;
      pending = true;
      try {
        while (active && wanted !== null) {
          const target = wanted;
          wanted = null;
          const value = await load(target);
          if (active && wanted === null) apply(value);
        }
      } catch (reason) {
        wanted = null;
        if (active) reportError(reason);
      } finally { pending = false; }
    },
  };
}
