# Design

**Flight deck for New Eden.** Tether looks like a ship's instrument panel: near-black "void" with warm bone text, hairline seams with bright corner registration marks, cut-corner controls, instruments instead of charts, and EVE's own colour meanings (standings blue and red) plus one signal orange for what needs you. The reference is the design canvas ("Tether: flight-deck identity for New Eden"): Identity, Dashboard, Character sheet, SRP, Sign in, Operations, Structures and Moon Mining artboards.

Every core page and every plugin page follows this file. When a screen needs something not covered here, extend this file first, then build it.

## Principles

- **Seams, not shadows.** Surfaces separate with 1px `--border` hairlines; panels carry bright corner brackets (registration marks). No drop shadows, glows or gradients, except the ambient sky (Motion).
- **Square, with one cut.** Corners are square. The primary button, the active tab and the active filter key have their bottom-right corner cut (8px, 7px on small controls). Nothing else is cut, so the cut marks what is selected or what acts.
- **EVE's colour meanings.** Blue (`--info`) is friendly, healthy, positive ISK; red (`--destructive`) is hostile, a loss, an error; the signal (`--accent`, orange by default) is the one thing that needs you now (the next countdown, pending counts, a queue about to end). Everything else is bone and fog.
- **Instruments, not charts.** Progress is a segmented bar (capacitor cells); skill levels are EVE's five squares; composition is a ring; readouts are mono numbers split by hairlines; facts are dotted-leader ledgers.
- **Numbers are monospaced.** Timers, ISK, counts, tickers, ids that are wanted, and timestamps use IBM Plex Mono so columns align and countdowns don't jitter.
- **Dense but readable.** Tables are the main way data is shown. Comfortable rows, overline headers, no zebra stripes.
- **Dark only.** Tokens are named so a light theme could come later without touching components.

## Tokens

Written as shadcn-style theme variables, so Basecoat's components pick them up.

```css
:root {
  /* surfaces */
  --background: #07090c;        /* void: the page */
  --sidebar: rgba(7,9,12,.8);   /* the shell over the sky */
  --card: #0c1016;              /* hull: panels, tables, cards */
  --muted: #121820;             /* raised: active key, tab track, subtle fills */
  --accent-surface: #0f141b;    /* hover rows, hover nav */

  /* text */
  --foreground: #e7e3d8;        /* bone: primary text */
  --foreground-soft: #a9afb7;   /* fog: secondary text, nav */
  --muted-foreground: #7c8591;  /* dim: labels, captions, overlines */
  --faint: #2a3645;             /* seams you notice: outlines, empty cells */

  /* lines */
  --border: #1c2530;            /* seam */
  --bracket: #566579;           /* panel corner marks */
  --input: #1c2530;
  --ring: #a9afb7;

  /* primary action: bone fill, void text */
  --primary: #e7e3d8;
  --primary-foreground: #07090c;

  /* signal: one per screen (per-instance setting) */
  --accent: #ff7a1a;
  --accent-soft: #2a1608;

  /* EVE standings */
  --info: #4da3ff;              /* friendly, healthy */
  --info-foreground: #9cc8ff;
  --info-soft: #0e1e33;
  --destructive: #f2555a;       /* hostile, loss, error */
  --destructive-soft: #2b1012;

  /* shape */
  --radius: 0;                  /* square everywhere */
  --cut: 8px;                   /* the one cut corner */
}
```

**With Basecoat.** shadcn's `accent` role is a hover surface, not a highlight. So Basecoat's `accent` utilities map to `--accent-surface` and `--foreground`, and the signal `--accent` is exposed as the `highlight` colour (`text-highlight`, `bg-highlight-soft`). The mapping lives in `assets/app.css`.

The signal colour is a per-instance setting. Alliances pick it under Admin → System → Appearance (presets or any colour); signal orange `#ff7a1a` is the default. A colour must reach 4.5:1 against `--background`, so it reads as text and carries dark text in pills; `--accent-soft` is derived. It reaches pages as `/theme.css`, loaded after the built stylesheet, since the CSP allows no inline styles.

**Moon ore rarity** (Moon Mining) is its own scale, brighter with value: R4 `#3c4a5c`, R8 `#6b7d92`, R16 `#a9afb7`, R32 `#e3c08a`, R64 the signal. Security status: 0.5 and up blue, above 0 signal, 0 and below red.

Status colours must differ in lightness as well as hue, and pair colour with a text label or a square mark, never colour alone.

## Typography

Fonts are **bundled and self-hosted**, never loaded from Google Fonts or any CDN (see the opsec rule in `CLAUDE.md`). Archivo (variable, with its width axis; subset to Latin, Latin Extended, Cyrillic and the symbols Tether uses) and IBM Plex Mono (IBM's variable woff2, unmodified), both under the SIL Open Font License (`assets/vendor/fonts/`).

```css
--font-sans: "Archivo", ui-sans-serif, system-ui, sans-serif;
--font-mono: "IBM Plex Mono", ui-monospace, monospace;
```

| Use | Size | Weight | Notes |
| --- | --- | --- | --- |
| Page title (h1) | 30px | 600 | Archivo at 125% width (`font-stretch`), -0.01em |
| Detail title (h2 in a profile or side panel) | 18–20px | 600 | 125% width |
| Overline (card headers, column headers, labels over readouts) | 11px | 500 | uppercase, +0.14em tracking, `--muted-foreground` |
| Body, table cells | 14px | 400 | primary text |
| Nav items, buttons | 14px | 500 | tabs and filter keys are overline-styled (11px, +0.14em, uppercase) |
| Captions, meta | 12px | 400 | `--muted-foreground` |
| Readouts (stat values) | 22px | 500 | Plex Mono; a unit suffix (`B`, `M`, `/3`) in `--muted-foreground` |
| Timers, ISK, counts, tickers | inherits | 400–500 | Plex Mono |

## Spacing and layout

A 4px base. Common steps: 4, 8, 12, 14, 18, 22, 26, 36.

| Element | Value |
| --- | --- |
| Status strip (top) | 40px, full width, bottom seam |
| Sidebar width | 236px, right seam, over the sky |
| Content padding | 30px top, 36px sides |
| Gap between page sections | 20px |
| Gap between panels | 14px |
| Panel padding | 18px |
| Right rail (detail panels) | 330–372px |
| Desktop design width | 1440px; layouts must hold down to 1280px |

Page structure: status strip (wordmark, EVE clock, Tranquility and ESI status, the page's trail, search, bell) → sidebar → page header (overline, title, and on the right readouts or the page's actions) → optional readouts or filter keys → content with an optional right rail.

## Components

**Status strip**: 40px, `--sidebar` over the sky. Left to right: the wordmark (the two-node tether glyph and `TETHER` in Archivo at 125% width, 700, +0.18em), then in 12px Plex Mono: `EVE 05:14:22` (ticking in the browser), `■ TRANQUILITY 24,112` (players online from ESI's public status, the square blue when up, red when down), `■ ESI` health, then the page's trail (section / page in uppercase mono, the current part in bone). On the right, the notification bell with its unread count in the signal. Below 768px it keeps only a menu button (which opens the sidebar), the wordmark, the EVE clock and the bell.

**Panels** (`.card`): `--card` fill, 1px `--border`, square, with four 10px corner brackets in `--bracket` (drawn as background images on the border box, so no extra markup). 18px padding. A panel's header is an overline, not a big title.

**Buttons**, 34px tall (28px small), square, 13px/500 (12px small), 16px horizontal padding.
- Primary: `--primary` fill, `--primary-foreground` text, weight 600, the bottom-right corner cut. At most one per screen region.
- Outline: `--card` fill, `--faint` border, `--foreground` text. The default for secondary actions; a "+ Add…" outline carries the cut too.
- Quiet: no border, `--muted-foreground` text, turning `--foreground` (or `--destructive` for Reject/Remove) on hover. For low-weight row actions such as Reject or Make main.
- Destructive: outline with `--destructive` text; filled `--destructive` only inside confirmations.
- Icon-only: 32×32 outline with an `aria-label`.

**Inputs**, 32px tall, `--card` fill, `--border` border, square, 13px. Every input has a `<label>`, visually hidden if the design omits it; an overline label beside a filter is fine.

**Readouts** (stat values): an overline label over a 22px mono value, in a row split by 1px vertical seams (no boxes). Used in page headers and under profiles. A value that needs you is in the signal; hostile or loss in red.

**Tables**: inside a panel. Header row is overline (11px, +0.14em, uppercase, `--muted-foreground`); body 13–14px; 10px vertical cell padding; 18px on the outer columns; 1px seams between rows; hover lightens the row. A status column is a leading 8px square in the status colour. Cells wrap between words, never inside one; a table still too wide for its panel scrolls sideways inside it, so the page itself never does. Row actions right-aligned: a quiet text action, then a small primary or outline button. Primary cell: a 28px framed portrait or icon, the name, and the ticker in 12px mono `--muted-foreground`.

**Tabs and filter keys**: a row of keys, 30px tall, 11px/600 uppercase, +0.14em. The active key has a `--faint` border, `--muted` fill, bone text and the cut corner; inactive keys are `--muted-foreground` text with no border. Counts follow the label in mono (`PENDING 03`), the count in the signal when it needs attention.

**Badges**: 10–11px mono or overline text, 1px border, square, 1px × 6px padding. Never filled: a status badge is its colour's border and text (the signal, `--destructive`), a neutral one `--faint` and `--foreground-soft`.
- Neutral: `--faint` border, `--foreground-soft` text.
- Status: the status colour's border and text (`--info`, `--destructive`, signal), or a soft fill for the state badge. Never colour alone: a word goes with it.
- Notification levels: danger red, warning signal, success blue, info neutral. The bell's unread count is the signal.

**Ledger** (facts in a panel or card): the label in `--muted-foreground`, a dotted `--faint` leader filling the space, the value right-aligned (mono for numbers). Used for character facts, membership, summaries.

**Segmented bar** (progress, fuel, a drill cycle): 16–32 cells, 2px apart, 5–6px tall; lit cells in bone (or the signal when it needs you soon, or a rarity colour), unlit `#161d26`. A bar between two known instants fills live.

**Skill levels**: five 9px squares: trained filled bone, the level in training outlined in the signal over `--accent-soft`, the rest outlined `--faint`.

**Countdowns**: `T− 3h 12m` in Plex Mono; the nearest one, or any under a few hours, in the signal; hostile timers in red.

**Sidebar navigation**: grouped under overline headings with a hairline running to the right. Items are 32px, 14px text in `--foreground-soft`, indented 26px. The active item: `--muted` fill, bone text at 500, and a 6px signal square before its label; `aria-current="page"`. Counts are mono in the signal (`03`). The sidebar sits over the sky with `--sidebar`, as tall as the window, and stays put while the page scrolls; its links scroll inside it if they must. Below 768px it leaves the page to the content and opens over it from the status strip's menu button, full height with the page dimmed behind it (the browser's popover again: no script; Escape or a tap outside closes it). The signed-in character sits at the bottom above a seam (framed 30px portrait, name, `[TICKER] STATE` in mono) and opens the account menu: a small panel above it (the browser's own popover: no script, Escape or a click outside closes it) with Token Management, Access tokens and Log out. Those live only there, never in the sidebar or elsewhere. Services is a sidebar item in Account instead, as in AA (where pilots look to link Discord), for those with a service to link. Each section folds from its heading: the heading is the toggle, with a 10px chevron after the hairline (turned down while open, half opacity until hovered). The browser remembers which sections it folded in a preference cookie (`tether_nav_folded`, written by `assets/live.js`), so the server draws the next page already folded, with nothing flashing; the section holding the page being shown is always open. Admins arrange the sections, items, folders (a collapsible item, open while one of its pages is shown) and custom links on the Menu page; the default is Account, Fleet, Industry, Corporation, Apps and Admin, and a section with nothing this person may open isn't shown. Apps' links go in the section their manifest names (Fleet for Fleet Activity Tracking, Fleet Operations, Ship Replacement, Structure Timers and Fittings; Industry for Moon Mining and Structures; Corporation for Member Audit and HR Applications), or Apps. AA's officer tools are sidebar items for whoever holds their permissions, as in AA, and stay in the Administration hub too: Corporation Stats and the Compliance Report open the Corporation section, and Permissions Audit sits in Admin under Administration. Admin otherwise holds one item, Administration (see below); every other admin page starts hidden in the sidebar, and admins can pin any of them on the Menu page. A menu an admin has saved keeps its arrangement: new defaults apply only to items it has no entry for.

**Portraits and logos**: square, in a 1px `--faint` frame with 2px padding at 54px and up; the corporation logo overlaps a portrait's bottom-right corner as a 20px square with a 2px `--card` ring. Initials on `--muted` when there is no picture.

**Watermark**: on pages showing sensitive data, a single line in 11px Plex Mono, `--faint`, bottom-right of the data panel: `Viewing as <character> · <EVE time>`. Decorative, `aria-hidden`. Only where it matters: app pages showing members' data. Never on the Dashboard or its widgets.

**Toasts** confirm an action or explain why it didn't happen, without moving the page: bottom-right, at most 400px wide, 24px from the edges. Each toast is a bracketed panel with 12px padding (no shadow): a 6px square (blue for done, red for a problem), one line of 13px text, and a quiet Undo or close. Done toasts leave after 4 seconds, problems after 8, and hovering keeps them. New ones stack above older ones, three at most. They fade in and rise 4px over 150ms (none under reduced motion). `role="status"`, or `role="alert"` for problems. The server asks for one with an `HX-Trigger` header (`{"toast": {"message": "...", "tone": "done" | "problem"}}`); `assets/live.js` draws it with text only, never markup.

**Icons**: Lucide-style outline icons, 16px, stroke width 2, `currentColor`, square caps where the icon allows. Bundled as inline SVG, never fetched. No emoji.

## Page hygiene and state

Every element earns its place. The feel to aim for is a quiet, dense modern app, not a page of links.

- **One primary action per context, and no duplicates.** A page offers each action once. If the card grid offers Register Character, the page header doesn't also offer Add character.
- **No stray links.** A link lives where its target makes sense: a heading that is itself the link, a row, a card, or the page header's own links. No trailing "Open", "More", "Details" or "Groups" words at the edge of a section; section headers never carry bare utility links. A small secondary button belongs beside what it acts on.
- **Progressive disclosure.** Long lists (scopes, raw errors, audit details, permissions) are summarised ("29 scopes · all required granted") behind a disclosure, or link to the page that owns them. Scopes live on Token Management and Register Character, not the Dashboard.
- **No raw ids.** People see names (or tickers), never database or EVE ids, unless the id is the thing being looked for (an app id, a job number). An EVE name that isn't known yet reads "Unknown corporation" (or alliance), not a number.
- **State is preserved.** Tabs, search, filters, sort and pagination live in the URL query, so back and forward restore them and a link shares them. Switching tabs swaps only the content (htmx, `hx-push-url`) and keeps the scroll position; a search carries across the tabs of the same page. After any action (a row button, a form, Run now, Approve) the viewer stays on the same page, tab, query and scroll position, with a toast saying it's done or why not. An action never lands on a separate "result" page, unless the result is a new thing with its own page (a new group). Without JavaScript, forms still post and the browser is sent back to the page.

How the platform keeps it (`crates/web-core/src/pages/stay.rs`, `assets/live.js`): a boosted form post that answers with a redirect back to the page it came from is turned into an in-place reload of that page, keeping its query and scroll; one that answers with an error page becomes a problem toast instead; one that answers with a page is swapped in place without touching the address. A boosted link or GET form to the page already shown (a tab, a filter, the next page) swaps in place too. Only a move to another page scrolls to the top. In-place swaps don't replay the page's arrival motion.

## Dashboard

The pilot's landing page. With Member Audit installed and its basic access, it is the pilot's character audit: the page header (title and one line; no button, since the grid registers characters), then Member Audit's My Characters widget (its heading links to the app; its combined totals as stat cards, then the card grid, "Register another character" first). Under each of the account's characters the host adds a footer: a status chip (Registered, or "Missing 3 scopes", which links to Register Character) and, for a character with working EVE access that isn't the main, a quiet "Make main" button. Under the grid, one small secondary "Change Main with EVE login" for a character without working access. Then Membership (the state badge, then the groups as chips linking to Groups), then other apps' widgets, then the viewer's permissions behind a disclosure. Without Member Audit, or without access to it, the header carries Add character, and the summary stat row (state, characters, groups, permissions) and a compact Characters table (portrait, name, corporation, alliance, status chip, Main marker or Make main) come first instead. No watermark on the Dashboard or its widgets.

## Administration

Admin pages live in one place instead of filling the sidebar. The sidebar's Administration item opens an overview (`/admin`) with every admin page the viewer may open, grouped: **Access** (States, Groups, Auto Groups, Permissions, Permissions Audit), **Members** (Users, Blacklist, Compliance Report, Corporation Stats), **Integrations** (Discord, Fleet Pings, Apps) and **Instance** (System, Menu, Audit log, Setup). A group with nothing the viewer may open is left out. The list lives in `crates/web-core/src/admin_nav.rs`; a new admin page goes there, with a group and one sentence on what it does.

- **Overview**: page header; for holders of `admin.system`, the System panel (AA's Dashboard admin panels: Software Version, Task Queue, ESI and the error budget as four stat cards under a 15px/600 "System" heading that is itself the link to the System page), loaded after the page so a slow ESI never holds it up; then each group as a 15px/600 heading with a one-line muted description, over tiles two across (three from 1536px). A tile is a link: a 32px `--muted` square holding the page's icon, its name at 14px/500, and its sentence at 13px muted. Hover lightens it like any surface.
- **Rail**: every admin page (not the overview) shows a 208px second column right of the sidebar, cascading out of it: the same fill, a right border, its own 64px header ("Administration") in line with the top bar, then an Overview link and the groups under nav headings, each page a 32px link. The current page is marked like the sidebar's. Like the sidebar it is window-tall and stays put while the page scrolls. It shows from 1280px; narrower, the sidebar item and the overview are the way around.
- **Permissions** (`/admin/permissions`): a filter (the name or what it allows, in the URL), then every permission with its grants as chips (× revokes, after a confirmation). Each row's small ghost Edit opens its picker under the chips: a checklist of states, then groups, two columns, the current holders ticked, and Save. Save changes only what this admin changed (what was ticked when the page was drawn goes along), so a grant someone else made meanwhile stays; the toast names what was granted and revoked. For a sensitive permission, Guest, public and blacklist states and Open groups can't be ticked, with one muted line saying why. One picker is open at a time. Grants to single users are made on the user's page.
- **Apps** (`/admin/plugins`): Installed first (each app's version, status and one action: Manage, or Review update), then **Included with Tether** (the included apps not installed yet, each with Review and install), both by name. Only an app that didn't come with Tether carries a badge ("Third party"). Then installing from GitHub, and the pinned keys.
- The sidebar's Administration item stays marked on every page in the hub, except the officer tools with sidebar items of their own (Corporation Stats, the Compliance Report, Permissions Audit), which mark their own item. Their breadcrumbs name their section: "Corporation / Corporation Stats".

## Confirm it's you

Sudo mode's interstitial (`/reauthenticate`), shown when an owner-only or sensitive action needs a fresh EVE login. It uses the bare layout of the login page (no sidebar): one card, 400px wide.

- **Header**: h2 "Confirm it's you", then "Log in with EVE again to continue: **<action>**", the action named in plain words ("Uninstall an app"), in `--foreground` at 500.
- **Body**: one muted 14px paragraph: the login must be from the last 15 minutes, with which character (the account's main, by name), and that the admin comes back to the page they were on to submit again. Nothing is sent for them.
- **Actions**: the primary full-width button "Log in with EVE Online" (a form post), and under it a full-width outline "Cancel" back to that page.
- No countdowns, warnings or accent: it's a routine step, not an error.

## Motion

Motion says something arrived, is on its way, or that the ship is alive; it never gets in the way. Interface motion lasts 200ms or less (the progress line aside), eases out, and uses opacity and at most a 4px rise. Under `prefers-reduced-motion: reduce`, none of it happens, the sky included.

- **The sky** (ambient, behind every page): a navigation-chart dot grid (1px dots every 24px) panning slowly diagonally (24px per 16s, seamless), two sparse star layers drifting at different speeds for depth (110s and 240s per tile), a few glints fading in and out (5s), and a faint blue scan sweep top to bottom about every 12s. It lives in one fixed layer behind the page; panels are opaque, so it never moves under text. It is the only decoration allowed to move continuously, and it stops under reduced motion.
- **Page content** fades in and rises 4px over 180ms when a page arrives (not when it is swapped in place: a tab, a filter, an action). The status strip, sidebar and rail stay still. The page's sections follow each other 20ms apart (60ms at most), and the tiles, readout cards and grid cards within one 25ms apart (at most 100ms).
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

- **Page header**: the title, the one-line description under it, and on the right the page's own links (sub-pages such as "Skill Sets · Character Finder · Reports") as filter keys (see Tabs and filter keys), the current page marked, then at most one primary button (cut corner) that opens a page ("Create timer"). Links to sub-pages live here, never in "More" cards at the bottom.
- **Entities** (a character, corporation, alliance, faction, or an item or ship type) are their 20px square picture from CCP's image server, then the name at 14px: portraits for characters, logos for corporations, alliances and factions, icons for types (CCP's 32px icon). The host builds the image address from the kind and id; a plugin never supplies a URL. No id (0 or less) gets initials on `--muted`, as avatars do. In tables the picture sits before the name.
- **Row actions**: buttons that post (Approve, Reject, Close) sit in a table's cell as small 28px buttons, right-aligned, several side by side: quiet for low-weight ones (Reject turns red on hover), outline by default, primary (cut) only for the row's one main action. A destructive or far-reaching one asks first: the browser's own popover, centred over a dimmed page, 360px wide, a bracketed panel, 20px padding, with one sentence stating the consequence ("Its 4 members lose access"), then Cancel (outline) and the action, filled destructive or primary. No script; Escape or a click outside cancels. Core pages ask the same way: a form or button with `hx-confirm` (its sentence) opens one such panel from the layout, which `assets/live.js` fills as text, naming its button after the one that asked (its title, else its text) and filling it destructive when that one is destructive (or Delete, Remove, Revoke); the request goes only from that button, and Enter in a form's field asks too. Never the browser's own `confirm()` dialog.
- **Text to copy** (a fitting in EFT format, a list): a card with its title on the left of the header and a small outline Copy button on the right; the text in Plex Mono at 12px, exactly as given (spaces and line breaks kept), on `--background` with a 1px border, 12px padding, scrolling past 384px. The button reads "Copied" for 1.5s after copying; where the browser can't copy, it selects the text instead. The host's own single values to copy (a group's direct join link) use the same button and script: a read-only field with the outline Copy button beside it. So does an app's link to share (a FAT link's register page, an SRP fleet's request page): the app names one of its own pages and the host writes the full address from the site's address (as the join link's), 12px Plex Mono in the field, the small Copy button beside it, at most 384px wide.
- **Profile**: the top of a page about one character (or corporation). A bracketed panel with the 64–100px framed portrait or logo on the left, an overline over the name (the subtitle, e.g. "Capsuleer · alt of …"), the name as an h2 at 20–32px/600 in Archivo at 125% width, and on one line under it the corporation and alliance as 20px entities in `--foreground-soft`. Badges (neutral or status) follow the name. Under them, the facts: a grid of three columns (two below 1280px, four from 1536px), each an overline label over its 14px value (or dotted-leader ledger lines in narrow panels); numbers, ISK, times and countdowns in Plex Mono. Key numbers can instead be a readout row under the profile. It replaces a tall label/value card as a page's overview.
- **Card grid** (My Characters): compact profiles, one card per character, in a grid of bracketed cards at least 288px wide (as many across as fit), 14px apart. Each card: the 54–64px framed portrait with the corporation and alliance logos (20px, a 2px `--card` ring) overlapping its bottom-right corner, then beside it the name at 15px/600 (a link to the card's page, the portrait too), badges, an optional 12px muted subtitle, and the corporation and alliance names in 12px `--foreground-soft`. Under it, the facts as ledger lines (label, dotted leader, value right-aligned; Plex Mono for numbers, ISK and times), then training as a segmented bar with its countdown, then a mono footer (`SYNCED 4M AGO`). For an app that reads the characters pilots register for it (user scopes), the host may start the grid with its own **Register Character** card, which opens registering for that app: a dashed `--faint` card, a 64px `--muted` square holding a plus icon, "Register another character" at 15px/600 and one muted line. The app asks for it; it never gets a link outside its own pages.
- **Progress**: a segmented bar (see Components), full width of its cell, with an optional 12px muted label and the percentage in Plex Mono above it. A bar between two known instants (a skill in training) fills live in the browser. Bone by default; the signal only when the plugin marks it urgent; no animation between values (it moves as time does).
- **Skill levels**: EVE's five 9px squares (see Components), from `levels` (trained, and the level in training). Labelled for screen readers ("Level 4 of 5, training 5").
- **Composition** (a moon's ores): a ring of the parts, each part's arc its share of the whole, coloured by grade on the rarity scale (darker to brighter by value), around a `#1c2530` disc. In a table cell, 36px with a 4px ring. With a `center`, 180px with a 12px ring, the center words in 15px Plex Mono in the disc, and a legend beside it (a square in the part's colour, the name, the share in mono), or under it where there's no room. As a card field, the large ring is a figure: its label above it, no dotted leader. At most 8 parts. Labelled with each part and its share.
- **Defenses** (a structure): EVE's three-quarter rings, 44px: shield outside (blue), armor (fog), hull (bone), each on a `#161d26` track; a ring with damage turns red. The core is a 6px square, pulsing red (the alarm motion) when the plugin says so.
- **Timeline** (fleets, timers, moon chunks, a planner's proposals): a bracketed panel; an axis of day ticks (`MON 28`, 10px mono, dim); a 140px label column (overline, with an optional mono caption); lanes 88px tall with a seam between; each event a small bracket-less box (1px `--faint` border, `--card` fill) at its time, two rows per lane so neighbours don't collide, holding a glyph (a diamond, or a 3px bar for an event with an end, which then stretches to its end), the label and the time left. Tones: signal for "needs attention soon", red for hostile, blue for friendly. A proposal is dashed and breathes. Windows (prime time) are faint blue bands across every lane; a 1px signal line marks now, labelled `NOW`. Positions are classes in half-percent steps (the CSP allows no inline styles). At most 20 lanes, 50 events a lane, 60 windows, 60 days. On a narrow screen it keeps at least 640px and scrolls sideways inside its panel.
- **Tabs** are links carrying the page's query plus the host's `_tab`; htmx swaps only the content (`#plugin-content`), pushes the address and keeps the scroll position. **Forms and row actions** post to the page's own address (with its query and tab), and the answer replaces the content in place with a toast; a page the app answers with is shown there, under the same tab and query, and an app's redirect to the same page keeps the tab. A button in a Dashboard widget leaves the viewer on the Dashboard.
- **Live pages**: a page that says it is still filling in (a first sync) reloads its content in place every few seconds (5 to 300) while it says so, and stops once it doesn't. The content swaps without the fade and without scrolling, so the page doesn't blink; a page with a form never reloads under someone typing, and an audited page never reloads at all.

**Owners** (AA's Add Owner). For an app that reads corporation data through data sources, the host adds two things around the app's own content; the app never sees either.

- **Add owner**: an outline button with a plus icon at the right of the page header, on every page of the app. It shows for those who may add a character: holders of one of the app's `add_…` permissions (as AA's `add_refinery_owner`, `add_structure_owner`, `add_fatlink`; an app's `manage` doesn't, as in AA), and app admins, as long as they may open the app's main page. It is one EVE login, which comes back to that page, and the owner is in use at once (AA's, no approval). An app may also place the same button in its own content with its own words (FAT's "Log in with the fleet boss" on Create FAT Link): a small outline button, drawn only for those same people, whose login comes back to that page, followed by the host's own 12px muted line "Adds one of your characters as this app's owner, used at once · EVE asks for:" and the scopes as outline badges, as in the Owners card.
- **Owners card**: on the app's main page, after the app's content. Its header says in one line whose data the app reads, then a 12px muted disclosure ("3 ESI scopes · how owners work") holding how owners work and the scopes asked for, as outline badges. The table shows the character (portrait), the corporation (20px logo), who added it and when, and the status: active, changed corporation (add it again), or not used (its account is deactivated or blacklisted, or the character left the account that added it). App admins see every source, with Remove as a row button, and under the table the sources withdrawn or removed in the last 30 days. Everyone else sees only their own characters, with Withdraw. The card isn't shown to a viewer who can't add or manage owners and has none.
- **Token Management** lists the account's own owners in every app (an "App owners" card: character, app, status, Withdraw), so a pilot can always see and withdraw them, even without access to the app any more.

## Don't

- No drop shadows, gradients, glows or glassmorphism (the sky's faint sweep is the one gradient, and it sits behind everything).
- No rounded corners, and no cut corners except on the primary button and the active key.
- No more than one signal colour on a screen's key element; blue and red only for their meanings.
- No Google Fonts, CDNs or remote assets at runtime.
- No emoji in the UI.
- No interface animation longer than 200ms except the progress line, the sky and the two alarms, and none that ignores reduced motion.
- No zebra-striped tables or heavy table borders.
- No light-gray text below 4.5:1 contrast for anything a user needs to read.
