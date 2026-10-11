type ThumbnailTask = {
  key: string;
  assetId: string;
  started: boolean;
  subscribers: number;
  promise: Promise<ArrayBuffer>;
  resolve: (buffer: ArrayBuffer) => void;
  reject: (reason: unknown) => void;
};

export function createThumbnailLoader(
  read: (assetId: string) => Promise<ArrayBuffer>,
  { concurrency = 4, maxEntries = 128, maxBytes = 16 * 1024 * 1024 } = {},
) {
  const cached = new Map<string, ArrayBuffer>();
  const pending = new Map<string, ThumbnailTask>();
  const queue: ThumbnailTask[] = [];
  let cachedBytes = 0;
  let active = 0;
  const cancelled = () => new Error("缩略图请求已取消");

  function remember(key: string, buffer: ArrayBuffer) {
    if (!buffer.byteLength || buffer.byteLength > maxBytes) return;
    const previous = cached.get(key);
    if (previous) cachedBytes -= previous.byteLength;
    cached.delete(key);
    cached.set(key, buffer);
    cachedBytes += buffer.byteLength;
    while (cached.size > maxEntries || cachedBytes > maxBytes) {
      const oldest = cached.keys().next().value!;
      cachedBytes -= cached.get(oldest)!.byteLength;
      cached.delete(oldest);
    }
  }

  function pump() {
    while (active < concurrency && queue.length) {
      const task = queue.shift()!;
      task.started = true;
      active++;
      void Promise.resolve().then(() => read(task.assetId)).then((buffer) => {
        remember(task.key, buffer);
        task.resolve(buffer);
      }, task.reject).finally(() => {
        if (pending.get(task.key) === task) pending.delete(task.key);
        active--;
        pump();
      });
    }
  }

  return {
    load(assetId: string, revision: string, signal: AbortSignal): Promise<ArrayBuffer> {
      if (signal.aborted) return Promise.reject(cancelled());
      const key = JSON.stringify([assetId, revision]);
      const buffer = cached.get(key);
      if (buffer) {
        cached.delete(key);
        cached.set(key, buffer);
        return Promise.resolve(buffer);
      }
      let task = pending.get(key);
      if (!task) {
        let resolve!: ThumbnailTask["resolve"];
        let reject!: ThumbnailTask["reject"];
        const promise = new Promise<ArrayBuffer>((accept, fail) => { resolve = accept; reject = fail; });
        task = { key, assetId, started: false, subscribers: 0, promise, resolve, reject };
        pending.set(key, task);
        queue.push(task);
      }
      const request = task;
      const result = new Promise<ArrayBuffer>((resolve, reject) => {
        let settled = false;
        request.subscribers++;
        function finish() {
          if (settled) return false;
          settled = true;
          signal.removeEventListener("abort", abort);
          request.subscribers--;
          return true;
        }
        function abort() {
          if (!finish()) return;
          reject(cancelled());
          if (!request.started && request.subscribers === 0) {
            queue.splice(queue.indexOf(request), 1);
            pending.delete(key);
            request.reject(cancelled());
          }
        }
        signal.addEventListener("abort", abort, { once: true });
        request.promise.then((value) => { if (finish()) resolve(value); }, (reason) => { if (finish()) reject(reason); });
      });
      pump();
      return result;
    },
  };
}
