// Instant tooltips for icon buttons (`data-tip`): the native `title` waits about a second, and the tab strip clips
// anything drawn below it, so this one is positioned on screen instead.

import { el } from "./dom";

const tip = el("div", { className: "tip", role: "tooltip", hidden: true });
document.body.append(tip);

const hide = () => (tip.hidden = true);
document.addEventListener("mouseover", (e) => {
  const target = (e.target as Element).closest<HTMLElement>("[data-tip]");
  if (!target) return hide();
  tip.textContent = target.dataset.tip ?? "";
  tip.hidden = false;
  const r = target.getBoundingClientRect();
  const x = Math.min(Math.max(4, r.left + r.width / 2 - tip.offsetWidth / 2), window.innerWidth - tip.offsetWidth - 4);
  tip.style.cssText = `left:${x}px;top:${r.bottom + 6}px`;
});
document.addEventListener("mousedown", hide);
