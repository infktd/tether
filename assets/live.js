// App pages' live values (DESIGN.md, Plugins): countdowns tick, progress
// bars between two instants fill, and Copy buttons copy (app code blocks,
// and Group Management's direct join links). The server draws
// each as it is when the page is made; this only keeps them current, so
// the page reads fine without it. Nothing here animates: values change in
// place. Every instant comes from an attribute the host wrote.
(() => {
  // As plugin_pages.rs's countdown_text.
  const left = (seconds) => {
    if (seconds <= 0) return "done";
    const d = Math.floor(seconds / 86400);
    const h = Math.floor((seconds % 86400) / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    const s = seconds % 60;
    if (d > 0) return `${d}d ${h}h ${m}m`;
    if (h > 0) return `${h}h ${m}m`;
    if (m > 0) return `${m}m ${String(s).padStart(2, "0")}s`;
    return `${s}s`;
  };

  const tick = () => {
    const now = Date.now();
    for (const el of document.querySelectorAll("time[data-countdown]")) {
      const at = Date.parse(el.getAttribute("datetime"));
      if (Number.isNaN(at)) continue;
      const text = left(Math.floor((at - now) / 1000));
      if (el.textContent !== text) el.textContent = text;
    }
    for (const bar of document.querySelectorAll("progress[data-from][data-to]")) {
      const from = Date.parse(bar.dataset.from);
      const to = Date.parse(bar.dataset.to);
      if (!(to > from)) continue;
      const fraction = Math.min(1, Math.max(0, (now - from) / (to - from)));
      bar.value = fraction;
      const percent = `${Math.floor(fraction * 100)}%`;
      bar.textContent = percent;
      const shown = bar.parentElement && bar.parentElement.querySelector("[data-percent]");
      if (shown && shown.textContent !== percent) shown.textContent = percent;
    }
  };
  setInterval(tick, 1000);

  // Copy buttons: the text is the <pre> in the same block, or its
  // read-only field (a group's direct join link).
  document.addEventListener("click", (event) => {
    const button = event.target instanceof Element && event.target.closest("button[data-copy]");
    if (!button) return;
    const block = button.closest("[data-code]");
    const text = block && block.querySelector("pre, input");
    if (!text) return;
    const field = text instanceof HTMLInputElement;
    const select = () => {
      if (field) {
        text.select();
        return;
      }
      const selection = window.getSelection();
      if (selection) selection.selectAllChildren(text);
    };
    if (!navigator.clipboard || !window.isSecureContext) {
      select();
      return;
    }
    navigator.clipboard.writeText(field ? text.value : text.textContent).then(
      () => {
        if (!button.dataset.label) button.dataset.label = button.textContent;
        button.textContent = button.dataset.copiedLabel || "Copied";
        clearTimeout(Number(button.dataset.timer));
        button.dataset.timer = String(
          setTimeout(() => {
            button.textContent = button.dataset.label;
          }, 1500),
        );
      },
      select,
    );
  });
})();
