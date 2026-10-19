// Settings › Shortcuts: every command with its keys. Click a shortcut and press new keys to rebind it; a
// combination another command already uses is refused until you choose to move it over.
import { el } from "../dom";
import { bindings, commands, isMac, setRecording } from "./commands";
import { conflicts, label, resolve } from "./keys";

/** Draws the pane into `host`. `overrides` are the user's own bindings (id → keys); `store` keeps new ones (and
 *  applies them, so `bindings` reflects them before the pane redraws). */
export function renderShortcuts(host: HTMLElement, overrides: Record<string, string[]>, store: (o: Record<string, string[]>) => void) {
  const search = el("input", { type: "search", placeholder: "Search commands", className: "shortcut-search" });
  const clash = el("p", { className: "hint note error-text" });
  const note = el("p", { className: "hint note" });
  const list = el("div", { className: "shortcut-list" });
  const save = (next: Record<string, string[]>) => {
    note.replaceChildren();
    overrides = next;
    store(next);
    draw();
  };
  const set = (id: string, keys: string[] | undefined) => {
    const next = { ...overrides };
    if (keys === undefined) delete next[id];
    else next[id] = keys;
    save(next);
  };
  const defaults = (id: string) => (commands.find((c) => c.id === id)?.keys ?? []).map((k) => resolve(k, isMac)).filter(Boolean);
  const nameOf = (id: string) => commands.find((c) => c.id === id)?.label ?? id;

  const draw = () => {
    const q = search.value.toLowerCase();
    const shared = conflicts(Object.fromEntries(commands.map((c) => [c.id, bindings(c.id)])));
    list.replaceChildren(
      ...commands
        .filter((c) => `${c.menu} ${c.label}`.toLowerCase().includes(q))
        .map((c) => {
          const keys = bindings(c.id);
          const record = el("button", { type: "button", className: "keys", title: "Click, then press the new shortcut (Esc cancels)" },
            ...(keys.length ? keys.map((k) => el("kbd", { textContent: label(k, isMac), className: shared.has(k) ? "clash" : "" })) : ["None"]));
          record.onclick = () => {
            record.replaceChildren("Press keys…");
            record.classList.add("recording");
            setRecording((pressed) => {
              setRecording(null);
              if (!pressed) return draw();
              const owner = commands.find((o) => o.id !== c.id && bindings(o.id).includes(pressed));
              if (!owner) return set(c.id, [pressed]);
              note.replaceChildren(`${label(pressed, isMac)} is already used by ${nameOf(owner.id)}. `);
              const move = el("button", { type: "button", textContent: `Use it for ${c.label} instead` });
              move.onclick = () => {
                save({ ...overrides, [owner.id]: bindings(owner.id).filter((k) => k !== pressed), [c.id]: [pressed] });
              };
              note.append(move);
              draw();
            });
          };
          const clear = el("button", { type: "button", className: "ghost", textContent: "Clear", title: "No shortcut" });
          clear.onclick = () => set(c.id, []);
          const reset = el("button", { type: "button", className: "ghost", textContent: "Reset", title: "Back to the default" });
          reset.onclick = () => set(c.id, undefined);
          reset.disabled = !(c.id in overrides);
          const row = el("div", { className: "shortcut-row" }, el("span", { className: "menu-name", textContent: c.menu }),
            el("span", { className: "cmd", textContent: c.label }), record, clear, reset);
          row.title = defaults(c.id).length ? `Default: ${defaults(c.id).map((k) => label(k, isMac)).join(", ")}` : "No default shortcut";
          return row;
        }),
    );
    clash.textContent = shared.size
      ? `${[...shared.keys()].map((k) => label(k, isMac)).join(", ")} ${shared.size === 1 ? "is" : "are"} used twice; only the first command in this list gets it.`
      : "";
  };
  const resetAll = el("button", { type: "button", textContent: "Reset all shortcuts" });
  resetAll.onclick = () => save({});
  search.oninput = draw;
  host.replaceChildren(el("div", { className: "shortcut-head" }, search, resetAll), clash, note, list);
  draw();
}
