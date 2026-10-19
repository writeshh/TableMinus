// Fuzzy matching for Open Anything: the query's characters in order, anywhere in the text. No DOM, so the node
// tests can import it.

/** The positions of the query's characters in `text` and a score (higher is better), or null when it doesn't
 *  match. Runs of adjacent characters and matches at the start of a word score more, so "ord" ranks "orders"
 *  above "or_d_x" and "oi" finds "order_items". */
export function fuzzy(query: string, text: string): { score: number; at: number[] } | null {
  const q = query.toLowerCase().replace(/\s+/g, "");
  if (!q) return { score: 0, at: [] };
  // Jumping ahead to a word start can skip a character needed later; then plain left-to-right decides.
  return match(q, text, true) ?? match(q, text, false);
}

function match(q: string, text: string, preferWordStarts: boolean) {
  const t = text.toLowerCase();
  const wordStart = (j: number) => j === 0 || /[^a-z0-9]/.test(t[j - 1]) || (text[j] !== t[j] && text[j - 1] === t[j - 1]);
  const at: number[] = [];
  let score = 0;
  for (const ch of q) {
    const prev = at.length ? at[at.length - 1] : -2;
    let i = t.indexOf(ch, prev + 1);
    if (i < 0) return null;
    if (preferWordStarts && i !== prev + 1 && !wordStart(i)) {
      for (let j = t.indexOf(ch, i + 1); j >= 0; j = t.indexOf(ch, j + 1)) if (wordStart(j)) (i = j), (j = t.length);
    }
    score += (i === prev + 1 ? 5 : 1) + (wordStart(i) ? 3 : 0);
    at.push(i);
  }
  return { score: score - t.length / 100, at }; // shorter names win ties
}

/** `items` that match, best first. */
export function rank<T>(query: string, items: T[], text: (item: T) => string): { item: T; at: number[] }[] {
  return items
    .map((item) => ({ item, m: fuzzy(query, text(item)) }))
    .filter((x): x is { item: T; m: { score: number; at: number[] } } => !!x.m)
    .sort((a, b) => b.m.score - a.m.score)
    .map(({ item, m }) => ({ item, at: m.at }));
}
