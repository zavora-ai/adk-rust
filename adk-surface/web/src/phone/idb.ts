/**
 * The phone's device token, kept in IndexedDB (database `adk-home`, store `device`) so an
 * installed app keeps it across relaunches; the service worker reads it from the same place
 * (spec 3.3 §5.4). The web home's operator token stays in sessionStorage, apart from it.
 *
 * A script running in the page can read it, as it can the operator's token: it is scoped
 * (read, decide, stop), expiring and revocable, and the phone pairs again when the system
 * clears its storage.
 */

/** Where a token is kept. Tests give their own. */
export interface TokenStore {
  read(): Promise<string | undefined>;
  write(token: string): Promise<void>;
  clear(): Promise<void>;
}

export const DATABASE = "adk-home";
export const STORE = "device";
export const KEY = "token";

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE, 1);
    request.onupgradeneeded = () => {
      if (!request.result.objectStoreNames.contains(STORE)) request.result.createObjectStore(STORE);
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

async function run<T>(mode: IDBTransactionMode, act: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  const db = await open();
  try {
    return await new Promise<T>((resolve, reject) => {
      const request = act(db.transaction(STORE, mode).objectStore(STORE));
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
  } finally {
    db.close();
  }
}

/** The token in this browser's IndexedDB. Storage that is off reads as no token. */
export const idbTokens: TokenStore = {
  async read() {
    try {
      const value = await run("readonly", (store) => store.get(KEY));
      return typeof value === "string" && value ? value : undefined;
    } catch {
      return undefined;
    }
  },
  async write(token) {
    await run("readwrite", (store) => store.put(token, KEY));
  },
  async clear() {
    await run("readwrite", (store) => store.delete(KEY));
  },
};

/** A store in memory, for tests and previews. */
export function memoryTokens(initial?: string): TokenStore {
  let token = initial;
  return {
    async read() {
      return token;
    },
    async write(value) {
      token = value;
    },
    async clear() {
      token = undefined;
    },
  };
}
