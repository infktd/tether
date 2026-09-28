// App pages' live values (DESIGN.md, Plugins): countdowns tick, progress
// bars between two instants fill, and Copy buttons copy (app code blocks,
// and Group Management's direct join links). Also staying on the page
// through in-place swaps, and the toasts actions answer with. The server draws
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
    if (d > 0) return `T\u2212 ${d}d ${h}h ${m}m`;
    if (h > 0) return `T\u2212 ${h}h ${m}m`;
    if (m > 0) return `T\u2212 ${m}m ${String(s).padStart(2, "0")}s`;
    return `T\u2212 ${s}s`;
  };

  const pad = (n) => String(n).padStart(2, "0");
  const tick = () => {
    const now = Date.now();
    // The status strip's EVE clock (UTC).
    const d = new Date(now);
    const clock = `${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())}`;
    for (const el of document.querySelectorAll("[data-eve-clock]")) {
      if (el.textContent !== clock) el.textContent = clock;
    }
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
      // The segmented bar beside it: light as many cells as the fraction.
      const cells = bar.parentElement ? bar.parentElement.querySelectorAll(".segbar > span") : [];
      const lit = Math.round(fraction * cells.length);
      cells.forEach((cell, i) => {
        if ((i < lit) !== cell.hasAttribute("data-lit")) cell.toggleAttribute("data-lit", i < lit);
      });
    }
  };
  setInterval(tick, 1000);
  tick();

  // Staying on the page (DESIGN.md, Page hygiene and state). A boosted
  // link or GET form to the page already shown (a tab, a filter, the next
  // page) keeps the scroll position; the server says the same for its
  // answers to posts (HX-Reswap, HX-Location). Whatever is swapped in
  // place doesn't replay the page's arrival motion: <html data-stay> for a
  // moment, gone once that motion couldn't run anyway.
  const root = document.documentElement;
  const samePage = (path) => {
    try {
      return new URL(path, location.href).pathname === location.pathname;
    } catch {
      return false;
    }
  };
  document.addEventListener("htmx:beforeSwap", (event) => {
    const detail = event.detail;
    if (detail.target !== document.body) return;
    const request = detail.requestConfig || {};
    const path = detail.pathInfo && detail.pathInfo.requestPath;
    if (request.boosted && request.verb === "get" && !detail.swapOverride && samePage(path)) {
      detail.swapOverride = "innerHTML show:none";
    }
    if (/show:none/.test(detail.swapOverride || "")) root.dataset.stay = "";
  });
  document.addEventListener("htmx:afterSettle", (event) => {
    if (event.detail.target !== document.body || !("stay" in root.dataset)) return;
    setTimeout(() => {
      delete root.dataset.stay;
    }, 300);
  });

  // Toasts (DESIGN.md): the server asks for one with HX-Trigger
  // {"toast": {"message", "tone"}}. Text only, never markup.
  const reduced = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const dismiss = (toast) => {
    if (!toast.isConnected || toast.getAttribute("aria-hidden") === "true") return;
    toast.setAttribute("aria-hidden", "true");
    setTimeout(() => toast.remove(), reduced() ? 0 : 160);
  };
  const closeIcon = () => {
    const ns = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(ns, "svg");
    for (const [k, v] of Object.entries({
      viewBox: "0 0 24 24",
      fill: "none",
      stroke: "currentColor",
      "stroke-width": "2",
      "stroke-linecap": "round",
      "stroke-linejoin": "round",
      "aria-hidden": "true",
    })) {
      svg.setAttribute(k, v);
    }
    const path = document.createElementNS(ns, "path");
    path.setAttribute("d", "M18 6 6 18M6 6l12 12");
    svg.append(path);
    return svg;
  };
  document.addEventListener("toast", (event) => {
    const box = document.getElementById("toaster");
    const { message, tone } = event.detail || {};
    if (!box || typeof message !== "string" || message === "") return;
    const problem = tone === "problem";
    const toast = document.createElement("div");
    toast.className = "toast";
    toast.dataset.tone = problem ? "problem" : "done";
    toast.setAttribute("role", problem ? "alert" : "status");
    toast.setAttribute("aria-hidden", "false");
    const content = document.createElement("div");
    content.className = "toast-content";
    const dot = document.createElement("span");
    dot.className = "toast-dot";
    dot.setAttribute("aria-hidden", "true");
    const text = document.createElement("p");
    text.className = "toast-text";
    text.textContent = message;
    const close = document.createElement("button");
    close.type = "button";
    close.className = "toast-close";
    close.setAttribute("aria-label", "Dismiss");
    close.append(closeIcon());
    close.addEventListener("click", () => dismiss(toast));
    content.append(dot, text, close);
    toast.append(content);
    // The toaster stacks upwards: the newest is last, above the others.
    box.append(toast);
    const shown = [...box.children].filter((t) => t.getAttribute("aria-hidden") !== "true");
    for (const old of shown.slice(0, Math.max(0, shown.length - 3))) dismiss(old);
    let timer = 0;
    const arm = () => {
      timer = setTimeout(() => dismiss(toast), problem ? 8000 : 4000);
    };
    toast.addEventListener("mouseenter", () => clearTimeout(timer));
    toast.addEventListener("mouseleave", arm);
    arm();
  });

  // Confirmations (DESIGN.md, Row actions): an hx-confirm's question in
  // the designed popover instead of the browser's dialog. Its button is
  // named as the one that asked (its title, else its text), filled
  // destructive when that one is; the request goes only from it. Enter in
  // a form's field asks too, since htmx asks on every submit. Text only,
  // never markup. Escape or a click outside cancels.
  let pending = null;
  document.addEventListener("htmx:confirm", (event) => {
    const box = document.getElementById("confirm");
    if (!event.detail.question || !box || !box.showPopover) return;
    event.preventDefault();
    const asker =
      event.detail.triggeringEvent?.submitter ??
      (event.detail.elt.matches("button") ? event.detail.elt : null) ??
      event.detail.elt.querySelector?.("button[type=submit], button:not([type])");
    const label = (asker?.title || asker?.textContent || "").trim() || "Confirm";
    const danger =
      asker?.classList.contains("btn-destructive-outline") || /^(Delete|Remove|Revoke)\b/.test(label);
    const go = document.getElementById("confirm-go");
    document.getElementById("confirm-text").textContent = event.detail.question;
    go.textContent = label;
    go.dataset.variant = danger ? "destructive" : "primary";
    pending = event.detail;
    box.showPopover();
    box.querySelector("button")?.focus();
  });
  document.addEventListener("click", (event) => {
    if (event.target.closest?.("#confirm-go") && pending) {
      const confirmed = pending;
      pending = null;
      document.getElementById("confirm")?.hidePopover();
      confirmed.issueRequest(true);
    }
  });
  document.addEventListener("toggle", (event) => {
    if (event.target.id === "confirm" && event.newState === "closed") pending = null;
  }, true);

  // Permissions: one row's picker open at a time.
  document.addEventListener("toggle", (event) => {
    const picker = event.target;
    if (!(picker instanceof HTMLDetailsElement) || !picker.classList.contains("grant-picker") || !picker.open) return;
    for (const other of document.querySelectorAll("details.grant-picker[open]")) {
      if (other !== picker) other.open = false;
    }
  }, true);

  // Sidebar sections fold from their headings. A cookie remembers which
  // (a preference, read by the server so the next page arrives folded).
  // Clicks only: browsers also fire "toggle" for sections that load open.
  document.addEventListener("click", (event) => {
    const summary = event.target instanceof Element && event.target.closest("details.nav-section > summary");
    if (!summary) return;
    const name = summary.parentElement.dataset.section;
    if (!name) return;
    const folding = summary.parentElement.open;
    const cookie = document.cookie.match(/(?:^|; )tether_nav_folded=([^;]*)/);
    // Only what the server accepts, so nothing else lingers in it.
    const folded = (cookie ? cookie[1] : "").split("~").filter((r) => /^[A-Za-z0-9:_-]{1,64}$/.test(r) && r !== name);
    if (folding) folded.push(name);
    const secure = location.protocol === "https:" ? "; secure" : "";
    document.cookie = `tether_nav_folded=${folded.slice(-30).join("~")}; path=/; max-age=31536000; samesite=lax${secure}`;
  });

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
