// Redis text the view shows (redis.ts), kept apart from the DOM so it can be tested: replies as redis-cli prints
// them, and expiry times.

/** A key as redis.rs sends it: its text, and its bytes in hex when they aren't UTF-8. */
export type Key = { text: string; hex?: string | null };
export type Entry = { key: Key; type: string };

/** A reply as redis.rs sends it. */
export type Shown =
  | { kind: "status"; text: string }
  | { kind: "error"; text: string }
  | { kind: "int"; value: number }
  | { kind: "bulk"; text: string | null }
  | { kind: "array"; items: Shown[] | null };

/** A string as redis-cli quotes it. */
const quoted = (s: string) => `"${s.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n").replace(/\r/g, "\\r").replace(/\t/g, "\\t")}"`;

/** A reply as redis-cli prints it: numbered items, nested ones indented under their number. */
export function formatReply(r: Shown): string {
  switch (r.kind) {
    case "status": return r.text;
    case "error": return `(error) ${r.text}`;
    case "int": return `(integer) ${r.value}`;
    case "bulk": return r.text === null ? "(nil)" : quoted(r.text);
    case "array": {
      if (r.items === null) return "(nil)";
      if (!r.items.length) return "(empty array)";
      const width = String(r.items.length).length;
      return r.items.map((item, i) => {
        const prefix = `${String(i + 1).padStart(width)}) `;
        const [first, ...rest] = formatReply(item).split("\n");
        return [prefix + first, ...rest.map((l) => " ".repeat(prefix.length) + l)].join("\n");
      }).join("\n");
    }
  }
}

/** Time left as the view shows it; -1 is no expiry, -2 a key that's gone. */
export function formatTtl(ms: number): string {
  if (ms === -1) return "No expiry";
  if (ms < 0) return "Expired";
  const s = Math.ceil(ms / 1000);
  const parts: string[] = [];
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60), sec = s % 60;
  if (d) parts.push(`${d}d`);
  if (h) parts.push(`${h}h`);
  if (m) parts.push(`${m}m`);
  if (sec || !parts.length) parts.push(`${sec}s`);
  return parts.slice(0, 2).join(" ");
}

/** Keys grouped by their `:` namespaces, as folders. */
export type Folder = { path: string; folders: Map<string, Folder>; keys: Entry[]; count: number };
export function namespaces(keys: Entry[]): Folder {
  const root: Folder = { path: "", folders: new Map(), keys: [], count: 0 };
  for (const k of keys) {
    const parts = k.key.text.split(":");
    let at = root;
    at.count++;
    for (const part of parts.slice(0, -1)) {
      const path = at.path ? `${at.path}:${part}` : part;
      let next = at.folders.get(part);
      if (!next) at.folders.set(part, (next = { path, folders: new Map(), keys: [], count: 0 }));
      next.count++;
      at = next;
    }
    at.keys.push(k);
  }
  return root;
}
