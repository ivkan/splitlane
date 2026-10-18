// The computers this phone is paired with, in IndexedDB. Each record holds
// the phone's private key for that computer as a CryptoKey that cannot be
// exported: the browser keeps it and lets this page use it, nothing more.
//
// A browser may clear the store (iOS does when space runs short). That is
// the same as the computer having removed the phone: pair again.

const DB = "splitlane";
const STORE = "computers";

function db() {
  return new Promise((resolve, reject) => {
    const open = indexedDB.open(DB, 1);
    open.onupgradeneeded = () => open.result.createObjectStore(STORE, { keyPath: "id" });
    open.onsuccess = () => resolve(open.result);
    open.onerror = () => reject(open.error);
  });
}

async function run(mode, work) {
  const handle = await db();
  try {
    return await new Promise((resolve, reject) => {
      // Strict: a pairing is reported kept only once it is on disk. A
      // phone ends a page in the background without warning, and with the
      // default a record it had already shown as kept could be gone.
      const tx = handle.transaction(STORE, mode, { durability: mode === "readwrite" ? "strict" : "default" });
      const result = work(tx.objectStore(STORE));
      tx.oncomplete = () => resolve(result?.result);
      tx.onerror = () => reject(tx.error);
      tx.onabort = () => reject(tx.error);
    });
  } finally {
    handle.close();
  }
}

export async function computers() {
  const all = (await run("readonly", (s) => s.getAll())) ?? [];
  return all.sort((a, b) => a.pairedAt - b.pairedAt);
}

/// Keep `computer`, in place of any earlier pairing with the same computer:
/// pairing again replaces the old room and key.
export async function keep(computer) {
  // One transaction, so that two pairings finishing together leave one
  // record, not two.
  await run("readwrite", (s) => {
    const all = s.getAll();
    all.onsuccess = () => {
      for (const old of all.result) if (sameBytes(old.machine, computer.machine)) s.delete(old.id);
      s.put(computer);
    };
  });
}

export async function forget(id) {
  await run("readwrite", (s) => s.delete(id));
}

function sameBytes(a, b) {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}
