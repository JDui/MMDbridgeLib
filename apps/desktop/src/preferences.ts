const sessionPreferences = new Map<string, string | null>();

export function readPreference(key: string): string | null {
  if (sessionPreferences.has(key)) return sessionPreferences.get(key) ?? null;
  try { return window.localStorage.getItem(key); } catch { return null; }
}

export function writePreference(key: string, value: string | null): void {
  sessionPreferences.set(key, value);
  try {
    if (value === null) window.localStorage.removeItem(key);
    else window.localStorage.setItem(key, value);
    sessionPreferences.delete(key);
  } catch { /* Keep the current window usable when storage is unavailable. */ }
}
