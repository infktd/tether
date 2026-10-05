// App pages' live values (DESIGN.md, Plugins): countdowns tick, progress
// bars between two instants fill, and Copy buttons copy (app code blocks,
// and Group Management's direct join links). Also staying on the page
// through in-place swaps, and the toasts actions answer with. The server draws
// each as it is when the page is made; this only keeps them current, so
// the page reads fine without it. Nothing here animates: values change in
// place. Every instant comes from an attribute the host wrote.
(() => {
  // Pages are never kept in the browser (htmx-config's historyCacheSize is
  // 0: Back asks the server, which checks the session). A cache an older
  // Tether left behind goes.
  try {
    localStorage.removeItem("htmx-history-cache");
  } catch (_) {}

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

  // Popups (DESIGN.md, Popups): a button opening one of the page's forms
  // puts its hidden values into the form, its sentence at the top, and
  // shows it; Cancel, Escape or a click outside closes it. The form posts
  // as any (boosted), and the page stays put with a toast.
  document.addEventListener("click", (event) => {
    const target = event.target instanceof Element ? event.target : null;
    const opener = target && target.closest("button[data-opens]");
    if (opener) {
      const dialog = document.getElementById(opener.dataset.opens);
      if (!(dialog instanceof HTMLDialogElement)) return;
      dialog.querySelector("form")?.reset();
      const box = dialog.querySelector("[data-popup-fields]");
      let fields = {};
      try {
        fields = JSON.parse(opener.dataset.fields || "{}");
      } catch (_) {}
      box.replaceChildren(
        ...Object.entries(fields).map(([name, value]) => {
          const input = document.createElement("input");
          input.type = "hidden";
          input.name = name;
          input.value = String(value);
          return input;
        }),
      );
      const lead = dialog.querySelector("[data-popup-lead]");
      if (lead) {
        if (!("text" in lead.dataset)) lead.dataset.text = lead.textContent;
        lead.textContent = opener.dataset.lead || lead.dataset.text;
      }
      dialog.showModal();
      dialog.querySelector("input:not([type=hidden]), select, textarea")?.focus();
      return;
    }
    if (target && target.closest("dialog.popup [data-close]")) {
      target.closest("dialog").close();
      return;
    }
    // A click on the backdrop (the dialog itself, not its card).
    if (target instanceof HTMLDialogElement && target.classList.contains("popup")) target.close();
  });
  // Posted (the page is swapped in place): a popup left open closes. A
  // page with a popup form never reloads itself (any form stops that).
  document.addEventListener("htmx:beforeRequest", (event) => {
    const dialog = event.detail.elt instanceof Element && event.detail.elt.closest("dialog.popup");
    if (dialog && dialog.open) dialog.close();
  });

  // Tables (DESIGN.md, Tables): a column heading sorts the rows shown
  // (again to reverse), and a long table gets a filter box that hides the
  // rows not matching as you type. All in the browser: nothing is fetched,
  // and a table Tether pages sorts and filters the page it shows. Group
  // headings and pagers (rows with a colspan or a heading cell) stay put.
  const dataRow = (row) => !row.querySelector("th, td[colspan]");
  const sortKey = (cell) => {
    const text = (cell ? cell.dataset.sort ?? cell.textContent : "").trim();
    if (text === "") return [2, ""];
    // Numbers, with thousands separators and k/M/B/T, ISK or %.
    const n = text.replace(/,/g, "").match(/^([-\u2212]?\d+(?:\.\d+)?)\s*([kmbt])?\s*(?:isk|%)?$/i);
    if (n) {
      const scale = { k: 1e3, m: 1e6, b: 1e9, t: 1e12 }[(n[2] || "").toLowerCase()] || 1;
      return [0, Number(n[1].replace("\u2212", "-")) * scale];
    }
    return [1, text.toLowerCase()];
  };
  const compare = (a, b) => {
    if (a[0] !== b[0]) return a[0] - b[0];
    if (a[0] === 0) return a[1] - b[1];
    return a[1].localeCompare(b[1], undefined, { numeric: true });
  };
  const sortBy = (heading) => {
    const table = heading.closest("table");
    const column = heading.cellIndex;
    const ascending = heading.getAttribute("aria-sort") !== "ascending";
    for (const other of heading.parentElement.children) other.removeAttribute("aria-sort");
    heading.setAttribute("aria-sort", ascending ? "ascending" : "descending");
    for (const body of table.tBodies) {
      const rows = [...body.rows];
      const first = rows.findIndex(dataRow);
      if (first < 0) continue;
      let end = first;
      while (end < rows.length && dataRow(rows[end])) end++;
      const sorted = rows.slice(first, end).sort((a, b) => {
        const [ka, kb] = [sortKey(a.cells[column]), sortKey(b.cells[column])];
        // Empty cells last, whichever way.
        if ((ka[0] === 2) !== (kb[0] === 2)) return ka[0] === 2 ? 1 : -1;
        const order = compare(ka, kb);
        return ascending ? order : -order;
      });
      const after = rows[end] || null;
      for (const row of sorted) body.insertBefore(row, after);
    }
  };
  const sortable = (heading) =>
    heading instanceof HTMLTableCellElement &&
    heading.closest("table.table > thead") &&
    heading.textContent.trim() !== "";
  document.addEventListener("click", (event) => {
    const heading = event.target instanceof Element && event.target.closest("th");
    if (heading && sortable(heading)) sortBy(heading);
  });
  document.addEventListener("keydown", (event) => {
    if ((event.key === "Enter" || event.key === " ") && sortable(event.target)) {
      event.preventDefault();
      sortBy(event.target);
    }
  });
  // Tables get keyboard-reachable headings, and a filter box from this
  // many rows.
  const FILTER_FROM = 8;
  const filter = (table, query) => {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    for (const body of table.tBodies) {
      let shown = 0;
      for (const row of body.rows) {
        if (!dataRow(row)) continue;
        const text = row.textContent.toLowerCase();
        const match = words.every((w) => text.includes(w));
        row.hidden = !match;
        if (match) shown++;
      }
      // A group (merged tables) with nothing left hides its heading too.
      for (const row of body.rows) {
        if (row.classList.contains("table-group")) row.hidden = words.length > 0 && shown === 0;
      }
    }
  };
  const enhance = (root) => {
    const tables = root.querySelectorAll ? root.querySelectorAll("table.table") : [];
    for (const table of tables) {
      if ("enhanced" in table.dataset) continue;
      table.dataset.enhanced = "";
      for (const heading of table.querySelectorAll(":scope > thead th")) {
        if (sortable(heading)) heading.tabIndex = 0;
      }
      const rows = [...table.tBodies].reduce((n, b) => n + [...b.rows].filter(dataRow).length, 0);
      if (rows < FILTER_FROM) continue;
      const box = document.createElement("div");
      box.className = "table-filter";
      const input = document.createElement("input");
      input.type = "search";
      input.className = "input";
      input.placeholder = "Filter these rows";
      input.setAttribute("aria-label", "Filter these rows");
      input.addEventListener("input", () => filter(table, input.value));
      box.append(input);
      table.before(box);
    }
  };
  document.addEventListener("htmx:load", (event) => enhance(event.detail.elt));
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", () => enhance(document));
  } else {
    enhance(document);
  }

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
