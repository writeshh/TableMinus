// Small DOM helpers shared by the UI modules.

export const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

/** An element with properties set and children appended. */
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> = {},
  ...kids: (Node | string)[]
): HTMLElementTagNameMap[K] {
  const n = Object.assign(document.createElement(tag), props);
  n.append(...kids);
  return n;
}

/** The saved connection's name in a workspace's session key: the backend keys a further tab on the same connection
 *  as name + "\u001f" + n (`Open` in lib.rs), and that suffix is never shown. */
export const connName = (key: string) => key.split("\u001f")[0];
