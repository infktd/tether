# Design

**Flight deck for New Eden.** Tether looks like a ship's instrument panel: near-black "void" with warm bone text, hairline seams with bright corner registration marks, cut-corner controls, instruments instead of charts, and EVE's own colour meanings (standings blue and red) plus one signal orange for what needs you. The reference is the design canvas ("Tether: flight-deck identity for New Eden"): Identity, Dashboard, Character sheet, SRP, Sign in, Operations, Structures and Moon Mining artboards.

Every core page and every plugin page follows this file. When a screen needs something not covered here, extend this file first, then build it.

## Principles

- **Seams, not shadows.** Surfaces separate with 1px `--border` hairlines; panels carry bright corner brackets (registration marks). No drop shadows, glows or gradients, except the ambient sky (Motion).
- **Square, with one cut.** Corners are square. The primary button, the active tab and the active filter key have their bottom-right corner cut (9px; 7px on small controls, 10px at 48px). Nothing else is cut, so the cut marks what is selected or what acts.
- **EVE's colour meanings.** Blue (`--info`) is friendly, healthy, positive ISK; red (`--destructive`) is hostile, a loss, an error; the signal (`--accent`, orange by default) is the one thing that needs you now (the next countdown, pending counts, a queue about to end). Everything else is bone and fog.
- **Instruments, not charts.** Progress is a segmented bar (capacitor cells); skill levels are EVE's five squares; composition is a ring; totals are stat cards of mono numbers; facts are dotted-leader ledgers.
- **Numbers are monospaced.** Timers, ISK, counts, tickers, ids that are wanted, and timestamps use IBM Plex Mono so columns align and countdowns don't jitter.
- **Dense but readable.** Tables are the main way data is shown. Comfortable rows, overline headers, no zebra stripes.
- **Dark only.** Tokens are named so a light theme could come later without touching components.

## Tokens

Written as shadcn-style theme variables, so Basecoat's components pick them up.

```css
:root {
  /* surfaces */
  --background: #07090c;        /* void: the page */
  --sidebar: rgba(7,9,12,.86);  /* the shell over the sky */
  --card: #0c1016;              /* hull: panels, tables, cards */
  --muted: #121820;             /* raised: active key, tab track, subtle fills */
  --accent-surface: #0f141b;    /* hover rows, hover nav */
  --selected: #111821;          /* the row a record panel belongs to */
  --track: #161d26;             /* unlit cells, rings' tracks, empty bars */
  --scrim: rgba(4,6,9,.72);     /* behind the command palette and dialogs */

  /* text */
  --foreground: #e7e3d8;        /* bone: primary text */
  --foreground-soft: #a9afb7;   /* fog: secondary text, nav, captions */
  --muted-foreground: #7c8591;  /* dim: labels, overlines, meta */
  --faint: #2a3645;             /* seams you notice: outlines, empty cells */
  --disabled: #5b6470;          /* controls that can't be used */

  /* lines */
  --border: #1c2530;            /* seam */
  --bracket: #566579;           /* panel corner marks */
  --input: #1c2530;
  --ring: #4da3ff;              /* focus: 2px, EVE blue */

  /* primary action: bone fill, void text */
  --primary: #e7e3d8;
  --primary-foreground: #07090c;

  /* signal: one per screen (per-instance setting) */
  --accent: #ff7a1a;
  --accent-soft: #2a1608;
  --accent-line: #5a3417;       /* a signal notice's border */
  --accent-wash: #170f09;       /* a signal notice's fill */

  /* EVE standings */
  --info: #4da3ff;              /* friendly, healthy */
  --info-foreground: #9cc8ff;
  --info-soft: #0e1e33;
  --destructive: #f2555a;       /* hostile, loss, error */
  --destructive-soft: #2b1012;
  --destructive-line: #5a2427;  /* a problem notice's border */
  --caution: #e8b04a;           /* stale: read long ago, may be out of date */

  /* shape */
  --radius: 0;                  /* square everywhere */
  --cut: 9px;                   /* the one cut corner */
  --cut-sm: 7px;                /* on small controls */
}
```

**With Basecoat.** shadcn's `accent` role is a hover surface, not a highlight. So Basecoat's `accent` utilities map to `--accent-surface` and `--foreground`, and the signal `--accent` is exposed as the `highlight` colour (`text-highlight`, `bg-highlight-soft`). The mapping lives in `assets/app.css`.

The signal colour is a per-instance setting. Alliances pick it under Administration → Settings → Appearance (presets or any colour); signal orange `#ff7a1a` is the default. A colour must reach 4.5:1 against `--background`, so it reads as text and carries dark text in pills; `--accent-soft` is derived. It reaches pages as `/theme.css`, loaded after the built stylesheet, since the CSP allows no inline styles.

**Moon ore rarity** (Moon Mining) is its own cold scale, brighter with value (the mock-up, 2026-10-06): R4 `#344152`, R8 `#4f6680`, R16 `--info`, R32 `--info-foreground`, R64 bone. The signal stays free for what needs you. Security status: 0.5 and up blue, above 0 signal, 0 and below red.

Status colours must differ in lightness as well as hue, and pair colour with a text label or a square mark, never colour alone.

## Typography

Fonts are **bundled and self-hosted**, never loaded from Google Fonts or any CDN (see the opsec rule in `CLAUDE.md`). Archivo (variable, with its width axis; subset to Latin, Latin Extended, Cyrillic and the symbols Tether uses) and IBM Plex Mono (IBM's variable woff2, unmodified), both under the SIL Open Font License (`assets/vendor/fonts/`).

```css
--font-sans: "Archivo", ui-sans-serif, system-ui, sans-serif;
--font-mono: "IBM Plex Mono", ui-monospace, monospace;
```

| Use | Size | Weight | Notes |
| --- | --- | --- | --- |
| Page title (h1) | 30px | 600 | Archivo at 125% width (`font-stretch`), -0.01em; 26px on phones |
| Detail title (h2 in a profile or side panel) | 18–20px | 600 | 125% width |
| Panel title (h2 in a panel's header) | 15px | 600 | bone; its meta beside it in 11px Plex Mono capitals, `--muted-foreground` |
| Overline (column headers, labels over values, stat labels) | 11px | 500 | uppercase, +0.14em tracking, `--muted-foreground` |
| Body | 14px | 400 | primary text |
| Table cells | 13px | 400 | a row's name 13.5px/500 over an 11.5px muted line |
| Nav items | 13.5px | 400 | `--foreground-soft`; the views bar is 11px/400 capitals (600 for the current view) |
| Buttons | 13px | 500 | 600 on the primary; 12px on small ones |
| Captions | 12px | 400 | `--foreground-soft` under a stat, `--muted-foreground` elsewhere |
| Stat values | 30px | 400 | Plex Mono, line-height 1; a unit suffix (`ISK`, `B`, `/3`) in `--muted-foreground` |
| Status lines | 12.5px | 400 | 500 when it needs someone or is wrong |
| Timers, ISK, counts, tickers | inherits | 400–500 | Plex Mono |

## Spacing and layout

A 4px base. Common steps: 4, 8, 12, 14, 18, 22, 26, 36.

| Element | Value |
| --- | --- |
| Command bar (top) | 52px, full width, bottom seam |
| Sidebar width | 236px, right seam, over the sky |
| Content padding | 22px top, 28px sides |
| Gap between page sections | 20px |
| Gap between panels | 14px (18px side by side) |
| Gap between stat cards | 12px |
| Panel padding | 18px (16px in a stat card) |
| Right rail (detail panels) | 330–372px |
| Desktop design width | 1440px; layouts must hold down to 1280px |

**Site name**: an instance may name itself (its alliance's or corporation's name, one line of visible text, at most 50 characters, EVE's own limit), asked on the setup wizard's last step and changed on Settings; both forms say the sign-in page is public. Tether stays Tether: the wordmark never changes. The name joins browser tab titles between the page and Tether ("Dashboard · Some Alliance · Tether"), and on the sign-in page it replaces the overline above the headline, as a 13px/600 uppercase line in bone after a 6px signal square. Error pages keep "· Tether" alone.

Page structure (the mock-up, 2026-10-06: https://claude.ai/artifact/Edvv5nBKef4vUMpyFwrxbZ): command bar → sidebar → trail → page header (eyebrow, icon, title; on the right the one primary action and Manage) → views bar → notices → toolbar → content, with an optional record panel on the right. Core pages and every app's pages are built from the same parts, so nothing tells them apart.

## Components

**Command bar**: 52px, `--sidebar` over the sky, a bottom seam; one row that wraps on narrow screens. Left to right: the wordmark (the two-node tether glyph and `TETHER` in Archivo at 125% width, 700, +0.18em), the instance's site name as a 32px outline chip (a 20px `--muted` square with its initials, the name at 13px/500), the search field (32px, up to 460px wide, `--card` fill, a search icon, "Search pages, pilots, moons, actions" in `--muted-foreground` and a `⌘K` key; it opens the command palette), then on the right in 11.5px Plex Mono `EVE 21:04:17` (ticking in the browser), `■ TQ 24,112` (players online, the square blue when up, red when down) and `■ ESI NOMINAL` in a 1px `--border` box (the square red and the word changed when ESI isn't), the bell (its unread count a 14px signal square over its corner), and the signed-in character (a 26px framed portrait and the name at 13px) that opens the account menu. Below 768px it keeps only the menu button (which opens the sidebar), the wordmark, a search button, the bell and the portrait. The page's trail is not in the bar: it heads the content.

**Trail**: the first line of the content, 11px Plex Mono uppercase in `--muted-foreground`, parts split by a `/` in `--faint`, the current page in bone, earlier parts linking where they are pages ("INDUSTRY / MOON MINING / MOONS").

**Page header**: under the trail, on every page, core and app alike. On the left a 44px icon tile (1px `--faint` frame, `--card` fill, the 22px icon in bone at stroke 1.5), then an eyebrow (11px overline in the signal: the app's name, or the core section's) over the title (Archivo at 125% width, 30px/600). On the right, at most one primary button (cut corner) and, for those who run the app, an outline Manage button with a sliders icon. A Manage page's eyebrow reads "<App> · Manage", the app's name linking back to it. A page's description, where it has one, is one line of 13px `--foreground-soft` under the title (two at most; more is cut); long explanations go in a panel or a disclosure, not the header. Everything hangs from the top: the icon tile and the buttons sit level with the title, so a longer or missing description never moves them, and the title keeps to one line (on a phone it may wrap, and the buttons go under it).

**Views bar**: under the page header, the views of an app (or a core area's pages) as one row of links: 11px/400 uppercase at +0.14em, 10px × 14px, `--muted-foreground`, hovering to `--muted`. The current view is bone at 600 with a 2px signal underline (`box-shadow: inset 0 -2px 0`), and `aria-current="page"`. A count follows a label in mono (`EXTRACTIONS 2`), in the signal when it needs someone. A 1px seam runs under the whole row. On phones it scrolls sideways with a 40px fade at its right edge. There is one views bar per page: never a second row of look-alike tabs.

**View chips**: a segmented group (1px `--border`, `--card` fill, 32px) for choosing which part of a list is shown (Owned · All · My surveys; Upcoming · Past), each a button with its count in mono; the chosen one has a `--border`-coloured fill (#1c2530), bone text at 500 and `aria-pressed="true"`. They live in the toolbar, kept in the address like any filter.

**Toolbar**: above a list, one wrapping row, 8px apart, 34px tall: the search box (up to 360px, a search icon, a `/` key that focuses it; the search runs on the server and lives in the address), filter chips (an applied filter is a `--muted` chip with a 1px `--faint` border, its name, its value in bone and a × that removes it; "+ Filter" is a dashed chip that opens the list of filters), view chips, then pushed right the icon buttons (columns) and an outline CSV button. The browser's row filter is this same search box: it filters the rows shown at once and asks the server on Enter or a pause. No list has a search card above it.

**Notice**: a one-line strip under the views bar for something that stops the page working as it should (a data source lost its role, ESI is down): a 1px border and a dark fill in the notice's colour (signal: `#5a3417` on `#170f09`), its icon, one sentence that says what happened and what it affects, and one outline button that goes to the fix. `role="status"`. At most one per page; more go in the bell.

**Record panel**: a selected row's details beside its list, 320–420px wide, a bracketed panel: a header (overline with the kind, the name at 18px Archivo 125%, a line of context, a 28px close button), the record's figure where it has one (a moon's composition ring at 164px with the value in its centre and the parts listed beside it), the facts as a ledger, and at the bottom the record's primary action and one outline action. The row it belongs to keeps a 2px signal bar on its left edge and a `#111821` fill. On narrow screens it drops under the list. Opening it changes the address, so it is shareable and survives a reload.

**Save bar**: a settings page saves once, from a bar fixed to the bottom of the content that appears when something changed: an 8px signal square, "3 unsaved changes", the sections they're in, then Discard (outline) and Save changes (primary). Each changed setting says CHANGED in 10px signal mono beside it. Leaving the page with unsaved changes asks first.

**Command palette** (`⌘K`, `Ctrl K`, or the search field): a 680px bracketed panel 96px from the top over a dimmed page: the search input (17px, the caret in the signal, `ESC`), scope chips (All, Pages, the apps' records, Pilots, Actions), then results grouped under overlines, each a 28px icon tile, a title, a muted line of context and an optional key; the highlighted one has a `--muted` fill and a 2px signal bar. A footer shows the keys (↑ ↓ move, ↵ open, TAB filter) and "Searches only what you may open". The server answers each search with only what the viewer may open; nothing is indexed or sent elsewhere.

**Keys** (`.kbd`): 10.5px Plex Mono in a 1px `--faint` box with a 2px bottom edge, `--card` fill, `--foreground-soft` text.

**Panels** (`.card`, `.bk`): `--card` fill, 1px `--border`, square, with four 10px corner brackets in `--bracket` (drawn as background images on the border box, so no extra markup). 18px padding. A panel's title is 15px/600 in bone; what qualifies it (a count, LIVE, EVE TIME, SINCE TETHER STARTED) goes beside it on the right as 11px Plex Mono capitals in `--muted-foreground` (`.panel-head` holding the title and a `.panel-meta`). Overlines label things inside a panel, never the panel itself.

**Buttons**, 34px tall (28px small), square, 13px/500 (12px small), 16px horizontal padding.
- Primary: `--primary` fill, `--primary-foreground` text, weight 600, the bottom-right corner cut. At most one per screen region.
- Outline: `--card` fill, `--faint` border, `--foreground` text. The default for secondary actions; a "+ Add…" outline carries the cut too.
- Quiet: no border, `--muted-foreground` text, turning `--foreground` (or `--destructive` for Reject/Remove) on hover. For low-weight row actions such as Reject or Make main.
- Destructive: outline with `--destructive` text; filled `--destructive` only inside confirmations.
- Icon-only: 32×32 outline with an `aria-label`.

**Inputs**, 32px tall, `--card` fill, `--border` border, square, 13px. Every input has a `<label>`, visually hidden if the design omits it; an overline label beside a filter is fine.

**Stat cards** (`.readouts` of `.card.stat`; the mock-up, 2026-10-06): bracketed panels 12px apart, each 16px inside: an 11px overline, the value in 30px Plex Mono (line-height 1, a unit suffix muted), and a 12px caption in `--foreground-soft`. A value that needs you is in the signal; hostile or loss in red. Three or four stay one row, and four become two rows of two on a narrower page; on a phone two to a row, an odd last card across both, so no card is ever left alone under the rest. The page's totals go here, above its panels.

**Readout grid** (counts inside a panel: ESI's responses, the job queue): `dl.readout-grid`, even columns at least 150px wide, each cell an 11px overline over an 18px mono value, 12px × 18px padding, 1px seams between rows. A label that wraps keeps its value on the row's line, so a row of numbers always lines up. A count that needs you is in the signal once it's above zero; one that must stay zero (ESI's 420s) in red. Use it instead of a one-row table: a table's columns size to their contents and never line up with the next table's.

**Shared components** (`templates/ui.html`): the markup core pages and app sections draw alike lives in macros there (`status`, `verdict`), so one change reaches every page. A core page draws a status with `ui::status`, never its own badge markup.

**Editing in place**: on core pages where one change touches one place (a group's members and leaders, a permission's row, Discord role mappings and ping channels, a state's covers and scopes), the change swaps only that place: the row, the card's list, and what it moves (a count beside a heading, every state's account count). The rest of the page stays exactly as it was: a table's sort and filter, an open picker, what's typed in another form. A row removed leaves; a form that adds to a list redraws the list and clears itself. Mark the region `data-in-place` with an id (or list, in the attribute, the `#id`s to swap, the first being the region, and `.class`es standing for every element on the page with that class and an id); everything else about the form is as ever, and without JavaScript it's the plain post and redirect. Changes that reorder or reshape the page (moving a state, renaming it) redraw the page in place, keeping the scroll position.

**Popups**: a row action that needs a little input (how many runs) opens a small form in a dialog over the dimmed page instead of a page of its own: the action's sentence on top, the fields, Cancel and the primary button at the bottom right. Escape, Cancel or a click outside closes it; posting it keeps the page where it was, with a toast. Apps get it by pointing an action at one of the page's forms.

**Live pages**: an app's open page fetches its content again in place once the app's data changed (one of its jobs, or someone's form, wrote something), told over the same live stream as the bell, at most every ten seconds and only for apps the viewer may open. Nothing blinks or moves: scroll position, tab, query and each table's sort and filter stay. It never refreshes under someone: while a field has focus or holds unposted changes, or a popup or confirmation is open, it waits; a page left meanwhile isn't fetched. A page showing a post's problem, or an audited page, isn't refreshed. Timed reloads are for pages whose content moves with the clock alone.

**Tables**: inside a panel. Header row is overline (11px, +0.14em, uppercase, `--muted-foreground`); body 13–14px; 10px vertical cell padding; 18px on the outer columns; 1px seams between rows; hover lightens the row. A status column is a leading 8px square in the status colour. Cells wrap between words, never inside one; a table still too wide for its panel scrolls sideways inside it, so the page itself never does. Row actions right-aligned: a quiet text action, then a small primary or outline button. Primary cell: a 28px framed portrait or icon, the name, and the ticker in 12px mono `--muted-foreground`. Column headings sort the rows shown, in the browser (numbers by value, text alphabetically; again to reverse; an arrow in `--primary` says which way), and a table of eight rows or more gets a "Filter these rows" box above it that hides rows not matching every word typed. Neither asks the server: a paged table sorts and filters the page it shows, and app pages still offer their own search for everything.

**Tabs and filter keys**: replaced by the views bar (navigation between pages) and view chips (which part of a list); see both above.

**Badges** are labels (a fit's type, Main, a timer's kind): 10–11px mono or overline text, 1px border, square, 1px × 6px padding, never filled: `--faint` border and `--foreground-soft` text, or the signal for the one highlighted. A number is never a badge.

**Status lines** say how something is (Jay, 2026-10-06): a 7px square in the tone's colour and the word in 12.5px, no border: working or done in `--info` with the word in `--foreground-soft`, needing someone in the signal, a problem in `--destructive` (the word in the same colour), like the command bar's ESI NOMINAL. A fourth tone, `off`, is for what isn't in use (not set up, switched off, not yet): a `--faint` square and the word in `--muted-foreground`. An app's badge with a success, warning or danger tone is drawn as one; a neutral or highlighted badge stays a badge. A status that links somewhere (Missing 1 scope · Register) underlines on hover.
- Never colour alone: a word goes with it. The state badge keeps its soft fill.
- Notification levels: danger red, warning signal, success blue, info neutral. The bell's unread count is the signal.

**Ledger** (facts in a panel or card): the label in `--muted-foreground`, a dotted `--faint` leader filling the space, the value right-aligned (mono for numbers). Used for character facts, membership, summaries.

**Segmented bar** (progress, fuel, a drill cycle): 16–32 cells, 2px apart, 5–6px tall; lit cells in bone (or the signal when it needs you soon, or a rarity colour), unlit `#161d26`. A bar between two known instants fills live.

**Skill levels**: five 9px squares: trained filled bone, the level in training outlined in the signal over `--accent-soft`, the rest outlined `--faint`.

**Countdowns**: `T− 3h 12m` in Plex Mono; the nearest one, or any under a few hours, in the signal; hostile timers in red.

**Sidebar navigation**: grouped under overline headings with a hairline running to the right. Items are 32px: a 16px icon (stroke 1.6; each app's from its manifest, each core page its own, never one shared placeholder), then the label at 13.5px in `--foreground-soft`, then a count right-aligned in 11px mono (`--muted-foreground`, or the signal when it needs someone). The active item: `--muted` fill, bone text at 500 and a 2px signal bar on its left edge (`box-shadow: inset 2px 0 0`); `aria-current="page"`. An app's item stays active on every page of the app (the `[[navigation]]` entry covering it). The sidebar sits over the sky with `--sidebar`, as tall as the window, and stays put while the page scrolls; its links scroll inside it if they must. Below 768px it leaves the page to the content and opens over it from the status strip's menu button, full height with the page dimmed behind it (the browser's popover again: no script; Escape or a tap outside closes it). The signed-in character sits at the right of the command bar and opens the account menu: a small panel under it (the browser's own popover: no script, Escape or a click outside closes it) with Act as, Token Management, Access tokens and Log out. Those live only there, never in the sidebar or elsewhere. The sidebar's foot holds, for those who run apps with data sources, a link to their health (a segmented bar, one cell per data source, blue when working and signal when not, over "11 working · 1 needs a role"), and in 10.5px mono the version and whether it is current (`TETHER 1.0.0 · UP TO DATE`). Services is a sidebar item in Account instead, as in AA (where pilots look to link Discord), for those with a service to link. Each section folds from its heading: the heading is the toggle, with a 10px chevron after the hairline (turned down while open, half opacity until hovered). The browser remembers which sections it folded in a preference cookie (`tether_nav_folded`, written by `assets/live.js`), so the server draws the next page already folded, with nothing flashing; the section holding the page being shown is always open. Admins arrange the sections, items, folders (a collapsible item, open while one of its pages is shown) and custom links on the Menu page; the default is Account, Fleet, Industry, Corporation, Apps and Admin, and a section with nothing this person may open isn't shown. Apps' links go in the section their manifest names (Fleet for Fleet Activity Tracking, Fleet Operations, Ship Replacement, Structure Timers and Fittings; Industry for Moon Mining and Structures; Corporation for Member Audit and HR Applications), or Apps. AA's officer tools are sidebar items for whoever holds their permissions, as in AA, and stay in the Administration hub too: Corporation Stats and the Compliance Report open the Corporation section, and Permissions Audit sits in Admin under Administration. Admin otherwise holds one item, Administration (see below); every other admin page starts hidden in the sidebar, and admins can pin any of them on the Menu page. A menu an admin has saved keeps its arrangement: new defaults apply only to items it has no entry for.

**Portraits and logos**: square, in a 1px `--faint` frame with 2px padding at 54px and up; the corporation logo overlaps a portrait's bottom-right corner as a 20px square with a 2px `--card` ring. Initials on `--muted` when there is no picture.

**Watermark**: on pages showing sensitive data, a single line in 11px Plex Mono, `--faint`, bottom-right of the data panel: `Viewing as <character> · <EVE time>`. Decorative, `aria-hidden`. Only where it matters: app pages showing members' data. Never on the Dashboard.

**Toasts** confirm an action or explain why it didn't happen, without moving the page: bottom-right, at most 400px wide, 24px from the edges. Each toast is a bracketed panel with 12px padding (no shadow): a 6px square (blue for done, red for a problem), one line of 13px text, and a quiet Undo or close. Done toasts leave after 4 seconds, problems after 8, and hovering keeps them. New ones stack above older ones, three at most. They fade in and rise 4px over 150ms (none under reduced motion). `role="status"`, or `role="alert"` for problems. The server asks for one with an `HX-Trigger` header (`{"toast": {"message": "...", "tone": "done" | "problem"}}`); `assets/live.js` draws it with text only, never markup.

**Icons**: Lucide-style outline icons, 16px, stroke width 2, `currentColor`, square caps where the icon allows. Bundled as inline SVG, never fetched. No emoji.

## Page hygiene and state

Every element earns its place. The feel to aim for is a quiet, dense modern app, not a page of links.

- **One primary action per context, and no duplicates.** A page offers each action once. If the roster offers Register another character, the page header doesn't also offer Add character.
- **No stray links.** A link lives where its target makes sense: a heading that is itself the link, a row, a card, or the page header's own links. No trailing "Open", "More", "Details" or "Groups" words at the edge of a section; section headers never carry bare utility links. A small secondary button belongs beside what it acts on.
- **Progressive disclosure.** Long lists (scopes, raw errors, audit details, permissions) are summarised ("29 scopes · all required granted") behind a disclosure, or link to the page that owns them. Scopes live on Token Management and Register Character, not the Dashboard.
- **No raw ids.** People see names (or tickers), never database or EVE ids, unless the id is the thing being looked for (an app id, a job number). An EVE name that isn't known yet reads "Unknown corporation" (or alliance), not a number.
- **State is preserved.** Tabs, search, filters, sort and pagination live in the URL query, so back and forward restore them and a link shares them. Switching tabs swaps only the content (htmx, `hx-push-url`) and keeps the scroll position; a search carries across the tabs of the same page. After any action (a row button, a form, Run now, Approve) the viewer stays on the same page, tab, query and scroll position, with a toast saying it's done or why not. An action never lands on a separate "result" page, unless the result is a new thing with its own page (a new group). Without JavaScript, forms still post and the browser is sent back to the page.

How the platform keeps it (`crates/web-core/src/pages/stay.rs`, `assets/live.js`): a boosted form post that answers with a redirect back to the page it came from is turned into an in-place reload of that page, keeping its query and scroll; one that answers with an error page becomes a problem toast instead; one that answers with a page is swapped in place without touching the address. A boosted link or GET form to the page already shown (a tab, a filter, the next page) swaps in place too. Only a move to another page scrolls to the top. In-place swaps don't replay the page's arrival motion.

## Dashboard

The pilot's landing page: their characters (Jay, 2026-10-06), and nothing else. With Member Audit installed and its basic access, it is Member Audit's My characters, drawn as the Dashboard: the page header (icon tile, eyebrow "Account", title "Dashboard", and under it the account's state badge and its first four groups as outline chips, then "N more", each linking to Groups; "Browse groups" when there are none), Member Audit's views bar as on its own pages (My characters, then Skill sets, Character finder, Reports and Data export, each only for whoever may open it: a pilot without officers' permissions sees no bar), its totals as stat cards, then the roster (Card grid, below), and under it one small secondary "Change Main with EVE login" for a character without working access. Under each of the account's characters the host adds its status (Registered as a 6px blue square and the word, or a red chip such as "Missing 3 scopes", which links to Register Character) and, for a character with working EVE access that isn't the main, a quiet "Make main" button. The app's own main page is the Dashboard (its address shows it), its sidebar link goes, and its other pages keep the Dashboard marked in the sidebar and lead back to it. No other app adds anything to it, and the account's permissions are on Groups, behind a disclosure. Without Member Audit, or without access to it, the same header carries Add character, over a compact Characters table (portrait, name, corporation, alliance, status chip, Main marker or Make main). No watermark on the Dashboard.

## Administration

Admin pages live in one place instead of filling the sidebar. The sidebar's Administration item opens an overview (`/admin`) with every admin page the viewer may open, grouped: **Access** (States, Groups, Auto Groups, Permissions, Permissions Audit), **Members** (Users, Blacklist, Compliance Report, Corporation Stats), **Integrations** (Discord, Fleet Pings, Apps) and **Instance** (Health, Settings, Menu, Audit log, Setup). A group with nothing the viewer may open is left out. The list lives in `crates/web-core/src/admin_nav.rs`; a new admin page goes there, with a group and one sentence on what it does.

- **Overview**: page header; for holders of `admin.system`, Health's verdict and whatever isn't simply working (AA's Dashboard admin panels: Software Version, Task Queue and ESI), under a "Health" overline that is itself the link to Health, loaded after the page so a slow ESI never holds it up; then each group as an overline with a hairline (as the sidebar's sections) and a one-line muted description, over tiles two across (three from 1536px). A tile is a link: a 32px `--muted` square holding the page's icon, its name at 14px/500, and its sentence at 13px muted. Hover lightens it like any surface.
- **Trail and eyebrow**: an admin page's trail is Administration, its group, then the page ("Administration / Instance / Health"), and its eyebrow is the group.
- **Health** (`/admin/system`, Jay, 2026-10-06: a status page, not badges): what's wrong first. A verdict band (a bracketed panel: a 10px square in the worst tone, the headline at 18px Archivo 125% ("All systems nominal", "Needs attention", "2 problems" in red), then "1 warning · checked 21:04 EVE" in 12px mono), then Systems: one line each for ESI (Tranquility's pilots online, Up or Down with why), ESI limits (Tether's own use: within limits, syncs paused under the bulk reserve, a rate-limit group held, or the error budget exceeded), Discord, the job queue, backups, apps' data sources and updates. A line is an 8px square in its tone, the name (linking to where it's set up) over one line of what it is or what's wrong, its reading in 12px mono, and the status word right-aligned in a 140px column, in the tone's colour (bone-soft when working). Under them, ESI's responses since start as a readout grid, the rate-limit groups (numbers right-aligned), the job queue (a readout grid, then the newest 20 dead jobs with Retry, "The newest 20 of 52"), the schedules (in plain words: "every hour", "every 5 minutes"; the last run as a status line and how long ago; the next as "in 4m"; Tether's own with Run now, an app's under the app's name, run from its Apps page) and Version (what runs, whether there's newer, Check now, Upgrade and Roll back). Nothing on it is invented: no uptime bars or graphs until Tether records their history.
- **Settings** (`/admin/settings`): what an admin sets for the whole instance, one form each: site name, appearance (the signal colour), notifications kept per user, and update checks. Health reads; Settings sets.
- **Views bar** (Jay, 2026-10-06: no second column of admin links): every admin page but the overview shows its group's pages (Access, Members, Integrations, Instance) as the views bar under its page header, only those the viewer may open, the page shown marked; a group with one such page draws no bar. The sidebar keeps Administration marked, and the overview lists every group.
- **Permissions** (`/admin/permissions`): a filter (the name or what it allows, in the URL), then every permission with its grants as chips (× revokes, after a confirmation). Each row's small ghost Edit opens its picker under the chips: a checklist of states, then groups, two columns, the current holders ticked, and Save. Save changes only what this admin changed (what was ticked when the page was drawn goes along), so a grant someone else made meanwhile stays; the toast names what was granted and revoked. For a sensitive permission, Guest, public and blacklist states and Open groups can't be ticked, with one muted line saying why. One picker is open at a time. Grants to single users are made on the user's page.
- **Apps** (`/admin/plugins`): Installed first (each app's version, status and one action: Manage, or Review update), then **Included with Tether** (the included apps not installed yet, each with Review and install), both by name. Only an app that didn't come with Tether carries a badge ("Third party"). Then installing from GitHub, and the pinned keys.
- The sidebar's Administration item stays marked on every page in the hub, except the officer tools with sidebar items of their own (Corporation Stats, the Compliance Report, Permissions Audit), which mark their own item. Their breadcrumbs name their section: "Corporation / Corporation Stats".

## Confirm it's you

Sudo mode's interstitial (`/reauthenticate`), shown when a superuser-only or sensitive action needs a fresh EVE login. It uses the bare layout of the login page (no sidebar): one card, 400px wide.

- **Header**: h2 "Confirm it's you", then "Log in with EVE again to continue: **<action>**", the action named in plain words ("Uninstall an app"), in `--foreground` at 500.
- **Body**: one muted 14px paragraph: the login must be from the last 15 minutes, with which character (the account's main, by name), and that the admin comes back to the page they were on to submit again. Nothing is sent for them.
- **Actions**: the primary full-width button "Log in with EVE Online" (a form post), and under it a full-width outline "Cancel" back to that page.
- No countdowns, warnings or accent: it's a routine step, not an error.

## Motion

Motion says something arrived, is on its way, or that the ship is alive; it never gets in the way. Interface motion lasts 200ms or less (the progress line aside), eases out, and uses opacity and at most a 4px rise. Under `prefers-reduced-motion: reduce`, none of it happens, the sky included.

- **The sky** (ambient, behind every page): a navigation-chart dot grid (1px dots every 24px) panning slowly diagonally (24px per 16s, seamless), two sparse star layers drifting at different speeds for depth (110s and 240s per tile), a few glints fading in and out (5s), and a faint blue scan sweep top to bottom about every 12s. It lives in one fixed layer behind the page; panels are opaque, so it never moves under text. It is the only decoration allowed to move continuously, and it stops under reduced motion.
- **Page content** fades in and rises 4px over 180ms when a page arrives (not when it is swapped in place: a tab, a filter, an action). The command bar and sidebar stay still, and the sidebar keeps its scroll position. Moving to another page cross-fades the content (the browser's view transition: the old page fades out over 90ms while the new one arrives), so there is never a blank frame; browsers without view transitions simply swap.
- **Back and Forward** feel like an app's: the page comes back at once from the tab's memory, where it scrolled to, without the arrival motion, and is fetched again only if it is more than two minutes old (then it is shown at once and replaced when the server answers). The memory is the tab's alone: never written to the browser's storage, gone on logout or when the tab closes. Nothing ever reloads the whole page. The page's sections follow each other 20ms apart (60ms at most), and the tiles, readout cards and grid cards within one 25ms apart (at most 100ms).
- **Popovers** ease in: the confirmation fades in and rises 4px over 150ms, the account menu over 120ms, the phone sidebar slides 8px in from its edge over 150ms, and their dimmed backdrops fade in. They close at once.
- **Disclosures** (cards that open, the sidebar's sections and folders) open and close over 150ms where the browser can animate to auto height, and snap elsewhere; their chevron turns with them.
- **Buttons** give 1px under the pointer while pressed.
- **Deferred panels** (a panel loaded after the page, such as Administration's System panel) hold their place while they load: the same heading and cards, with a muted dash where each value goes, so the page doesn't move when they arrive. No shimmer: placeholders are still.
- **Swapped content** (htmx fragments) fades in over 150ms.
- **Progress**: while a request runs, a 2px `--foreground-soft` line grows across the top of the page, appearing only after 150ms so quick requests show nothing.
- **Alarms**: a reinforced or attacked structure's core square pulses (1.6s); a breathing outline marks a proposed (not yet real) plan. Nothing else pulses.
- **Hover and state changes**: 150ms colour transitions.
- Counts and countdowns never animate between values: they change in place (Plex Mono keeps them from jittering).

## Data display

- **Relative times** for recent events ("1h 12m ago") with the absolute EVE time in a tooltip or sub-line.
- **Countdowns** in Plex Mono (`T− 2d 4h`), updated live from the server (server-sent events); the nearest one gets the accent color. A countdown to an instant that is already known (a skill finishing, a timer) ticks in the browser instead, from the bundled `assets/live.js`, with the absolute EVE time in its tooltip: `T− 2d 4h 13m` over a day, `T− 4h 13m` over an hour, `T− 13m 05s` under it. Once the instant passes it reads `done`.
- **EVE time (UTC)** everywhere, labeled "EVE".
- **ISK** abbreviated in tables (`1.24b`, `350.2m`), full value on hover or in detail views.
- **EVE names** (systems, moons, structures, characters) exactly as ESI returns them. Character portraits and corp or alliance logos come from CCP's image server at 32px in tables and 36px in the sidebar (20px as an app's entity values, 64px in a profile), with initials as the fallback.

**State badges** show an account's access state as a status badge: Member in the signal, Blue in info, Guest and admin-made states neutral. The label is always the state's name.

## Configuration pages

Admin settings must make sense without documentation open. Alliance Auth's settings didn't; ours do. Every configuration page follows these rules on top of the rest of this file.

- **Plain language, in terms of pilots.** Each setting carries one sentence on its effect ("Pilots whose main is in one of these get Member"), never internal names. Headings name the thing, not the table.
- **Ordered lists are cards in order.** When order matters (states), each item is a card, top first, with up and down icon buttons, and one sentence says how the order is used ("The highest match wins").
- **EVE entities are chips.** An alliance, corporation or character in a list is a chip: 20px logo or portrait, name, kind in muted text, and a remove icon button with an `aria-label`. They are added with an exact-name search whose results show the logo and kind before anything is added.
- **Live counts.** Next to each item, how many accounts it covers right now, in Plex Mono.
- **Preview before impact.** A change that would move any account's access shows a confirmation first, listing where accounts move ("12 accounts: Guest → Member") with Apply and Cancel. A change that moves nobody applies at once. Plain forms work without JavaScript; htmx only makes them smoother.
- **Built-ins are marked.** Built-in items that can't be renamed or removed carry a lock icon and a one-line reason instead of hidden or disabled buttons.
- **Works from defaults.** A fresh instance's defaults are usable as they are, and empty states say what to do next.
- **Consequences in confirmations.** Destructive actions state what will happen ("Its 4 accounts become Guest"), not "Are you sure?".

## Interaction and accessibility

- Real `<button>`, `<a href>`, `<input>` and `<label>` elements, never click handlers on divs.
- Visible focus: a 2px `--ring` outline with 2px offset on every focusable element.
- Text contrast of at least 4.5:1; `--muted-foreground` on `--background` passes, `--faint` is for decoration only.
- Hover states lighten surfaces by one step (`--card` → `--accent-surface`). Anything else that moves follows Motion above.

## Plugins

Plugins never ship their own styles. They return a declarative page description (headers, stat rows, tables, cards, tabs, forms, badges, profiles, card grids, row actions, text to copy, links to share, skill levels, compositions, defenses and timelines) and the host renders it with these components, so every plugin looks native. Any exception needs a documented reason and still uses these tokens.

- **Layout** (the host's, never the app's; `arrange` in `crates/web-apps/src/pages/plugin_pages.rs`): sections keep the app's order. Titled tables that follow each other with the same columns (four or more) are drawn as one table, each one's title a heading row (12px uppercase, a hairline above all but the first), so their columns line up: a fit's high, mid and low slots, rigs, drones and cargo. Narrow sections of a kind that follow each other go side by side, two to a row from 1024px (stacked below), each as tall as it needs: code blocks with code blocks (EFT beside Buy All), and tables of up to three columns with each other (a fit's doctrines beside its required skills, fresh moons beside old ones). A narrow section on its own stays full width.
- **App settings** live at an app's `settings` page (and `settings/...`), for whoever may open it (its `[[pages]]` rule): the first entry of the app's Manage menu, and a Settings button on the app's Administration page.
- **Long tables** in apps are paged by Tether, 25 rows at a time (Jay, 2026-09-30): "26–50 of 60" and Previous / Next under the table, each table on its own page (`_p0`, `_p1`… in the address, which the app never sees), keeping the scroll position.
- **App shell**: Tether draws every app's frame from its manifest, never the app. `[[views]]` are its views bar, in order, the first its main page ("Overview"); `[action]` is its one primary action, a cut-corner button in the header on every view but its own (a page's own primary link, such as a record's Edit, takes its place on that page); `[[manage]]` are the pages for those who run it, in a Manage menu (an outline button with a sliders icon) after Settings. Each entry shows only to whoever may open its page, so nobody sees a link they can't follow. On a Manage page the eyebrow reads "<App> · Manage" (the app's name links back), the trail adds MANAGE, the bar shows the Manage pages, and the header has no Manage button. The page header itself is the shared one (Page header, above).
- **Entities** (a character, corporation, alliance, faction, or an item or ship type) are their 20px square picture from CCP's image server, then the name at 14px: portraits for characters, logos for corporations, alliances and factions, icons for types (CCP's 32px icon). The host builds the image address from the kind and id; a plugin never supplies a URL. No id (0 or less) gets initials on `--muted`, as avatars do. In tables the picture sits before the name.
- **Row actions**: buttons that post (Approve, Reject, Close) sit in a table's cell as small 28px buttons, right-aligned, several side by side: quiet for low-weight ones (Reject turns red on hover), outline by default, primary (cut) only for the row's one main action. A destructive or far-reaching one asks first: the browser's own popover, centred over a dimmed page, 360px wide, a bracketed panel, 20px padding, with one sentence stating the consequence ("Its 4 members lose access"), then Cancel (outline) and the action, filled destructive or primary. No script; Escape or a click outside cancels. Core pages ask the same way: a form or button with `hx-confirm` (its sentence) opens one such panel from the layout, which `assets/live.js` fills as text, naming its button after the one that asked (its title, else its text) and filling it destructive when that one is destructive (or Delete, Remove, Revoke); the request goes only from that button, and Enter in a form's field asks too. Never the browser's own `confirm()` dialog.
- **Text to copy** (a fitting in EFT format, a list): a card with its title on the left of the header and a small outline Copy button on the right; the text in Plex Mono at 12px, exactly as given (spaces and line breaks kept), on `--background` with a 1px border, 12px padding, scrolling past 384px. The button reads "Copied" for 1.5s after copying; where the browser can't copy, it selects the text instead. The host's own single values to copy (a group's direct join link) use the same button and script: a read-only field with the outline Copy button beside it. So does an app's link to share (a FAT link's register page, an SRP fleet's request page): the app names one of its own pages and the host writes the full address from the site's address (as the join link's), 12px Plex Mono in the field, the small Copy button beside it, at most 384px wide.
- **Profile**: the top of a page about one character (or corporation). A bracketed panel with the 64–100px framed portrait or logo on the left, an overline over the name (the subtitle, e.g. "Capsuleer · alt of …"), the name as an h2 at 20–32px/600 in Archivo at 125% width, and on one line under it the corporation and alliance as 20px entities in `--foreground-soft`. Badges (neutral or status) follow the name. Under them, the facts: a grid of three columns (two below 1280px, four from 1536px), each an overline label over its 14px value (or dotted-leader ledger lines in narrow panels); numbers, ISK, times and countdowns in Plex Mono. Key numbers can instead be a readout row under the profile. It replaces a tall label/value card as a page's overview.
- **Card grid** (My characters): compact profiles, one per character. A grid of the account's characters (one that starts with the host's Register card, below) is a **roster**: the rows of one bracketed panel, a header band of overlines over the columns (Character, Location, Ship, Wallet, Skill points, Training, Last update), 1px seams between rows, hover lightening the row, and the whole row a link to the character's page (Make main and the status link stay on top). Each row: the 36px framed portrait, the name at 14px/600 with its badges, the corporation and alliance in 12px `--foreground-soft` under it; then the facts in their columns (numbers right-aligned in Plex Mono, training as a segmented bar with its skill and percentage); then the host's status and Make main. Without room, Last update goes first; under 720px each row stacks into a small card, the facts three abreast with their own overline labels. The **Register** card, for an app that reads the characters pilots register for it (user scopes), opens registering for that app: in a roster, a dashed row at the end (a 36px dashed square holding a plus icon, "Register another character" at 13.5px/500 and one muted line). The app asks for it; it never gets a link outside its own pages. Other card grids: bracketed cards at least 288px wide (as many across as fit), 14px apart, each with the 54–64px framed portrait (the corporation and alliance logos overlapping its bottom-right corner), the name at 15px/600, badges, an optional subtitle, and the facts as ledger lines.
- **Progress**: a segmented bar (see Components), full width of its cell, with an optional 12px muted label and the percentage in Plex Mono above it. A bar between two known instants (a skill in training) fills live in the browser. Bone by default; the signal only when the plugin marks it urgent; no animation between values (it moves as time does).
- **Skill levels**: EVE's five 9px squares (see Components), from `levels` (trained, and the level in training). Labelled for screen readers ("Level 4 of 5, training 5").
- **Composition** (a moon's ores): a ring of the parts, each part's arc its share of the whole, coloured by grade on the rarity scale (darker to brighter by value), around a `#1c2530` disc. In a table cell, 36px with a 4px ring. With a `center`, 180px with a 12px ring, the center words in 15px Plex Mono in the disc, and a legend beside it (a square in the part's colour, the name, the share in mono), or under it where there's no room. As a card field, the large ring is a figure: its label above it, no dotted leader. At most 8 parts. Labelled with each part and its share.
- **Defenses** (a structure): EVE's three-quarter rings, 44px: shield outside (blue), armor (fog), hull (bone), each on a `#161d26` track; a ring with damage turns red. The core is a 6px square, pulsing red (the alarm motion) when the plugin says so.
- **Timeline** (fleets, timers, moon chunks, a planner's proposals): a bracketed panel; an axis of day ticks (`MON 28`, 10px mono, dim); a 140px label column (overline, with an optional mono caption); lanes 88px tall with a seam between; each event a small bracket-less box (1px `--faint` border, `--card` fill) at its time, two rows per lane so neighbours don't collide, holding a glyph (a diamond, or a 3px bar for an event with an end, which then stretches to its end), the label and the time left. Tones: signal for "needs attention soon", red for hostile, blue for friendly. A proposal is dashed and breathes. Windows (prime time) are faint blue bands across every lane; a 1px signal line marks now, labelled `NOW`. Positions are classes in half-percent steps (the CSP allows no inline styles). At most 20 lanes, 50 events a lane, 60 windows, 60 days. On a narrow screen it keeps at least 640px and scrolls sideways inside its panel.
- **Tabs** are links carrying the page's query plus the host's `_tab`; htmx swaps only the content (`#plugin-content`), pushes the address and keeps the scroll position. **Forms and row actions** post to the page's own address (with its query and tab), and the answer replaces the content in place with a toast; a page the app answers with is shown there, under the same tab and query, and an app's redirect to the same page keeps the tab. A button on the Dashboard (the character audit's main page) leaves the viewer on the Dashboard.
- **Live pages**: a page that says it is still filling in (a first sync) reloads its content in place every few seconds (5 to 300) while it says so, and stops once it doesn't. The content swaps without the fade and without scrolling, so the page doesn't blink; a page with a form never reloads under someone typing, and an audited page never reloads at all.

**Data sources** (AA's owners and its Add Owner). For an app that reads corporation data through data sources (characters whose EVE login it reads through), the host adds two things around the app's own content; the app never sees either.

- **Add data source**: an outline button with a plus icon at the right of the page header, on every page of the app. It shows for those who may add a character: holders of one of the app's `add_…` permissions (as AA's `add_refinery_owner`, `add_structure_owner`, `add_fatlink`; an app's `manage` doesn't, as in AA), and app admins, as long as they may open the app's main page. It is one EVE login, which comes back to that page, and the data source is in use at once (AA's, no approval). An app may also place the same button in its own content with its own words (FAT's "Log in with the fleet boss" on New FAT link): a small outline button, drawn only for those same people, whose login comes back to that page, followed by the host's own 12px muted line "Adds one of your characters as this app's data source, used at once · EVE asks for:" and the scopes as outline badges, as in the Data sources card.
- **Data sources card**: on the app's main page, after the app's content. Its header says in one line whose data the app reads, then a 12px muted disclosure ("3 ESI scopes · how data sources work") holding how data sources work and the scopes asked for, as outline badges. The table shows the character (portrait), the corporation (20px logo), who added it and when, and the status: active, changed corporation (add it again), or not used (its account is deactivated or blacklisted, or the character left the account that added it). App admins see every source, with Remove as a row button, and under the table the sources withdrawn or removed in the last 30 days. Everyone else sees only their own characters, with Withdraw. The card isn't shown to a viewer who can't add or manage data sources and has none.
- **Token Management** lists the account's own data sources in every app (an "App data sources" card: character, app, status, Withdraw), so a pilot can always see and withdraw them, even without access to the app any more.

## Don't

- No drop shadows, gradients, glows or glassmorphism (the sky's faint sweep is the one gradient, and it sits behind everything).
- No rounded corners, and no cut corners except on the primary button and the active key.
- No more than one signal colour on a screen's key element; blue and red only for their meanings.
- No Google Fonts, CDNs or remote assets at runtime.
- No emoji in the UI.
- No interface animation longer than 200ms except the progress line, the sky and the two alarms, and none that ignores reduced motion.
- No zebra-striped tables or heavy table borders.
- No light-gray text below 4.5:1 contrast for anything a user needs to read.
