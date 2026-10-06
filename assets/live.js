// App pages' live values (DESIGN.md, Plugins): countdowns tick, progress
// bars between two instants fill, and Copy buttons copy (app code blocks,
// and Group Management's direct join links). Also staying on the page
// through in-place swaps, and the toasts actions answer with. The server draws
// each as it is when the page is made; this only keeps them current, so
// the page reads fine without it. Nothing here animates: values change in
// place. Every instant comes from an attribute the host wrote.
(() => {
  // Pages are never kept in the browser's storage (htmx-config's
  // historyCacheSize is 0; Back and Forward use the tab's memory, below).
  // A cache an older Tether left behind goes.
  try {
    localStorage.removeItem("htmx-history-cache");
  } catch (_) {}

  // As plugin_pages.rs's countdown_text: two units, the second padded.
  const left = (seconds) => {
    if (seconds <= 0) return "done";
    const d = Math.floor(seconds / 86400);
    const h = Math.floor((seconds % 86400) / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    const s = seconds % 60;
    const two = (n) => String(n).padStart(2, "0");
    if (d > 0) return `T\u2212 ${d}d ${two(h)}h`;
    if (h > 0) return `T\u2212 ${h}h ${two(m)}m`;
    if (m > 0) return `T\u2212 ${m}m ${two(s)}s`;
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
  // Moving to another page cross-fades it (DESIGN.md, Motion): the
  // browser's view transition, which htmx runs where the browser has one.
  // The sidebar keeps its scroll position across every page change.
  // A transition skipped (a second click before the first finished) still
  // swaps the page; only its animation is dropped, so that isn't an error.
  if (document.startViewTransition) {
    const start = document.startViewTransition.bind(document);
    document.startViewTransition = (update) => {
      const transition = start(update);
      transition.ready.catch(() => {});
      transition.finished.catch(() => {});
      return transition;
    };
  }
  const sidebarNav = () => document.querySelector(".app-sidebar nav");
  let sidebarTop = null;
  const keepSidebar = () => {
    sidebarTop = sidebarNav()?.scrollTop ?? null;
  };
  const putSidebar = () => {
    const nav = sidebarNav();
    if (nav && sidebarTop !== null) nav.scrollTop = sidebarTop;
    sidebarTop = null;
  };
  document.addEventListener("htmx:beforeSwap", (event) => {
    const detail = event.detail;
    if (detail.target !== document.body) return;
    const request = detail.requestConfig || {};
    const path = detail.pathInfo && detail.pathInfo.requestPath;
    if (request.boosted && request.verb === "get" && !detail.swapOverride) {
      // A hidden tab can't run a view transition (the browser refuses it).
      const transition = document.hidden ? "" : " transition:true";
      detail.swapOverride = samePage(path) ? "innerHTML show:none" : "innerHTML show:window:top" + transition;
    }
    if (/show:none/.test(detail.swapOverride || "")) root.dataset.stay = "";
    keepSidebar();
  });
  document.addEventListener("htmx:afterSwap", (event) => {
    if (event.detail.target === document.body) putSidebar();
  });
  document.addEventListener("htmx:afterSettle", (event) => {
    if (event.detail.target !== document.body || !("stay" in root.dataset)) return;
    setTimeout(() => {
      delete root.dataset.stay;
    }, 300);
  });

  // Back and Forward (DESIGN.md, Motion): the tab keeps the pages it has
  // shown in its memory, so going back shows one at once, where it was
  // scrolled to, without the arrival motion; one kept for more than two
  // minutes is shown at once and fetched again behind it. htmx's own
  // history cache, which would write pages to the browser's storage, stays
  // off: nothing here is written anywhere, and closing the tab or logging
  // out (a full navigation) forgets it all. Audited pages are never kept
  // (htmx doesn't offer pages marked hx-history="false").
  const FRESH_FOR = 120000;
  const KEEP = 12;
  const memory = new Map();
  // Where each page was scrolled to, kept even after its page is let go,
  // and always put back (at once, not smoothly): the browser's own restore
  // knows only positions from before htmx swapped the page.
  const scrolls = new Map();
  const scrollBack = (path) => {
    window.scrollTo({ top: scrolls.get(path) ?? 0, behavior: "instant" });
  };
  const keepable = (elt) => {
    // A filter box keeps its words with the rows they hid.
    for (const input of elt.querySelectorAll(".table-filter input")) input.setAttribute("value", input.value);
    const copy = elt.cloneNode(true);
    const transient = ["htmx-request", "htmx-settling", "htmx-swapping", "htmx-added"];
    for (const el of copy.querySelectorAll(transient.map((c) => "." + c).join(","))) el.classList.remove(...transient);
    for (const el of copy.querySelectorAll("[data-disabled-by-htmx]")) {
      el.removeAttribute("disabled");
      el.removeAttribute("data-disabled-by-htmx");
    }
    for (const dialog of copy.querySelectorAll("dialog[open]")) dialog.removeAttribute("open");
    return copy.innerHTML;
  };
  document.addEventListener("htmx:beforeHistorySave", (event) => {
    const { path, historyElt } = event.detail;
    memory.delete(path);
    memory.set(path, { content: keepable(historyElt), title: document.title, at: Date.now() });
    scrolls.set(path, window.scrollY);
    while (memory.size > KEEP) memory.delete(memory.keys().next().value);
  });
  document.addEventListener("htmx:historyCacheMiss", (event) => {
    root.dataset.stay = "";
    keepSidebar();
    const page = memory.get(event.detail.path);
    if (!page || !window.htmx) return;
    window.htmx.swap(
      event.detail.historyElt,
      page.content,
      { swapStyle: "innerHTML", swapDelay: 0, settleDelay: 0 },
      { contextElement: event.detail.historyElt, title: page.title },
    );
    putSidebar();
    scrollBack(event.detail.path);
    if (Date.now() - page.at >= FRESH_FOR) return;
    // Recent enough: nothing to fetch. htmx is told which page is shown,
    // as its own restore would, or it would keep the next one under the
    // wrong address; without session storage it fetches the page instead.
    try {
      sessionStorage.setItem("htmx-current-path-for-history", event.detail.path);
    } catch (_) {
      return;
    }
    event.preventDefault();
    setTimeout(() => {
      delete root.dataset.stay;
    }, 300);
  });
  document.addEventListener("htmx:historyRestore", (event) => {
    putSidebar();
    scrollBack(event.detail.path);
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

  // Editing in place (DESIGN.md, Editing in place): a form inside a
  // data-in-place region names it (its own id, or the list in the
  // attribute) to the server, which then swaps only those once it's done.
  // A .class stands for the ids of the elements on the page that have it
  // (htmx swaps the others by id alone).
  document.addEventListener("htmx:configRequest", (event) => {
    const elt = event.detail.elt;
    if (event.detail.verb === "get" || !(elt instanceof Element)) return;
    const region = elt.closest("[data-in-place]");
    if (!region) return;
    const listed = region.dataset.inPlace.trim() || (region.id ? `#${region.id}` : "");
    const names = listed.split(/\s+/).filter(Boolean).flatMap((name) => {
      if (!name.startsWith(".")) return [name];
      try {
        return [...document.querySelectorAll(`${name}[id]`)].map((el) => `#${el.id}`);
      } catch (_) {
        return [];
      }
    });
    if (names.length) event.detail.headers["HX-In-Place"] = names.join(" ");
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
  // A heading with words people see (not one only screen readers read,
  // as a row-actions column's).
  const sortable = (heading) =>
    heading instanceof HTMLTableCellElement &&
    heading.closest("table.table > thead") &&
    [...heading.childNodes].some((n) =>
      n.nodeType === Node.TEXT_NODE ? n.textContent.trim() !== "" : !n.classList?.contains("sr-only") && n.textContent.trim() !== "",
    );
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
    // A views bar wider than a phone's screen: the current view brought
    // into sight, without moving the page.
    for (const bar of root.querySelectorAll ? root.querySelectorAll(".views-bar") : []) {
      const current = bar.querySelector('[aria-current="page"]');
      if (!current || bar.scrollWidth <= bar.clientWidth) continue;
      const offset = current.getBoundingClientRect().left - bar.getBoundingClientRect().left;
      bar.scrollLeft += offset - (bar.clientWidth - current.offsetWidth) / 2;
    }
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
  // A row swapped in place under a filter is filtered like the rest.
  document.addEventListener("htmx:load", (event) => {
    const table = event.detail.elt instanceof Element && event.detail.elt.closest("table.table");
    const box = table && table.previousElementSibling;
    const query = box?.classList.contains("table-filter") ? box.querySelector("input").value : "";
    if (query) filter(table, query);
  });

  // An app page fetched again in place (its own timed reload, or a live
  // refresh below) keeps each table's sort and filter.
  let kept = null;
  const refetch = (detail) =>
    detail.target && detail.target.id === "plugin-content" && detail.requestConfig?.verb === "get" &&
    detail.requestConfig.elt?.id === "plugin-content";
  document.addEventListener("htmx:beforeSwap", (event) => {
    kept = null;
    // A reload answered with nothing (204) swaps nothing.
    if (!refetch(event.detail) || !event.detail.shouldSwap) return;
    kept = [...event.detail.target.querySelectorAll("table.table")].map((table) => {
      const heading = table.querySelector(":scope > thead th[aria-sort]");
      const box = table.previousElementSibling;
      return {
        column: heading ? heading.cellIndex : -1,
        descending: heading?.getAttribute("aria-sort") === "descending",
        query: box?.classList.contains("table-filter") ? box.querySelector("input").value : "",
      };
    });
  });
  document.addEventListener("htmx:load", (event) => {
    const content = event.detail.elt;
    if (!kept || !(content instanceof Element) || content.id !== "plugin-content") return;
    const tables = content.querySelectorAll("table.table");
    kept.forEach((was, i) => {
      const table = tables[i];
      if (!table) return;
      const heading = table.tHead?.rows[0]?.cells[was.column];
      if (heading && sortable(heading)) {
        sortBy(heading);
        if (was.descending) sortBy(heading);
      }
      const box = table.previousElementSibling;
      if (was.query && box?.classList.contains("table-filter")) {
        box.querySelector("input").value = was.query;
        filter(table, was.query);
      }
    });
    kept = null;
  });

  // Live app pages (DESIGN.md, Live pages): once an app's data changed,
  // an open page of it fetches its content again in place, at most every
  // REFRESH_GAP. Never under someone: while a field has focus or was
  // changed, or a popup is open, it waits, and a page left meanwhile
  // isn't fetched.
  const REFRESH_GAP = 10000;
  const RETRY = 3000;
  let changedApp = null;
  let timer = 0;
  let lastRefresh = 0;
  const live = () => document.querySelector("#plugin-content[data-app][data-href]");
  const edited = (field) => {
    if (field.closest(".table-filter") || field.closest("dialog:not([open])")) return false;
    if (field instanceof HTMLSelectElement) return [...field.options].some((o) => o.selected !== o.defaultSelected);
    if (field.type === "hidden") return false;
    if (field.type === "checkbox" || field.type === "radio") return field.checked !== field.defaultChecked;
    return field.value !== field.defaultValue;
  };
  const busy = (content) => {
    if (document.querySelector("dialog[open]")) return true;
    try {
      if (document.getElementById("confirm")?.matches(":popover-open")) return true;
    } catch (_) {}
    const active = document.activeElement;
    if (active && content.contains(active) && active.matches("input, select, textarea")) return true;
    return [...content.querySelectorAll("input, select, textarea")].some(edited);
  };
  const refresh = () => {
    timer = 0;
    const content = live();
    if (!changedApp || !content || content.dataset.app !== changedApp) {
      changedApp = null;
      return;
    }
    if (busy(content) || document.hidden) {
      timer = setTimeout(refresh, RETRY);
      return;
    }
    changedApp = null;
    lastRefresh = Date.now();
    window.htmx?.ajax("GET", content.dataset.href, {
      source: content,
      target: content,
      swap: "outerHTML show:none",
    });
  };
  // A post's answer is the page as it is now: the change it announces
  // waits its turn like any other.
  document.addEventListener("htmx:afterSwap", (event) => {
    const request = event.detail.requestConfig;
    if (event.detail.target?.id === "plugin-content" && request && request.verb !== "get") lastRefresh = Date.now();
  });
  document.addEventListener("app-changed", (event) => {
    const content = live();
    if (!content || content.dataset.app !== event.detail) return;
    changedApp = event.detail;
    if (!timer) timer = setTimeout(refresh, Math.max(500, lastRefresh + REFRESH_GAP - Date.now()));
  });

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
