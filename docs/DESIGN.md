# Design

The look is dark, quiet and dense: zinc neutrals, 1px borders instead of shadows, Geist for text, Geist Mono for anything that counts or ticks, and one accent color. Reference mockup: "Alliance Platform Mockup", Moon Tracker member view.

Every core page and every plugin page follows this file. When a screen needs something not covered here, extend this file first, then build it.

## Principles

- **Borders, not shadows.** Surfaces separate with 1px `--border` lines and a slightly lighter card fill. No drop shadows, glows or gradients.
- **One accent per screen.** The accent marks the single most important state (fresh moons, unread counts, the next countdown). Everything else is neutral.
- **Dense but readable.** Tables are the main way data is shown. Comfortable row height, clear column headers, no zebra stripes.
- **Numbers are monospaced.** Timers, ISK, counts, coordinates and timestamps use Geist Mono so columns align and countdowns don't jitter.
- **Dark mode only for v1.** Tokens are named so a light theme can be added later without touching components.

## Tokens

Written as shadcn-style theme variables, so they work unchanged with Basecoat, shadcn-svelte or shadcn/ui.

```css
:root {
  /* surfaces */
  --background: #09090b;        /* page */
  --sidebar: #0c0c0e;           /* sidebar, cards */
  --card: #0c0c0e;
  --muted: #18181b;             /* tab track, subtle fills */
  --accent-surface: #1c1c20;    /* active nav item, hover rows */

  /* text */
  --foreground: #fafafa;        /* primary text */
  --foreground-soft: #d4d4d8;   /* secondary text in tables and nav */
  --muted-foreground: #a1a1aa;  /* labels, captions, meta */
  --faint: #3f3f46;             /* watermarks, badge outlines */

  /* lines */
  --border: #27272a;
  --input: #27272a;
  --ring: #a1a1aa;

  /* primary action: inverted, white on dark */
  --primary: #fafafa;
  --primary-foreground: #09090b;

  /* accent: one per screen */
  --accent: #f59e0b;            /* amber */
  --accent-soft: #292012;       /* accent badge background */

  /* status */
  --info: #60a5fa;              /* Blue, healthy, informational */
  --info-foreground: #93c5fd;
  --info-soft: #13203a;
  --destructive: #ef4444;
  --destructive-soft: #2a1215;

  /* shape */
  --radius-sm: 6px;             /* buttons, inputs, badges, nav items */
  --radius: 8px;                /* tab track, avatars */
  --radius-lg: 10px;            /* cards, panels, tables */
}
```

**With Basecoat.** shadcn's `accent` role is a hover surface, not a highlight. So Basecoat's `accent` utilities (`bg-accent`, `text-accent-foreground`) map to `--accent-surface` and `--foreground`, and the amber `--accent` is exposed as the `highlight` colour (`text-highlight`, `bg-highlight-soft`). The mapping lives in `assets/app.css`.

The accent is a per-instance setting. Alliances pick it under Admin → System → Appearance (presets or any colour); amber is the default. Good alternatives keep similar lightness: `#fb923c`, `#a78bfa`, `#34d399`. A colour must reach 4.5:1 against `--background`, so it reads as text and carries dark text in pills; `--accent-soft` is derived (14% of the accent over the background). It reaches pages as `/theme.css`, loaded after the built stylesheet, since the CSP allows no inline styles.

Status colors must differ in lightness as well as hue, and pair color with a text label or dot, never color alone.

## Typography

Fonts are **bundled and self-hosted**, never loaded from Google Fonts or any CDN (see the opsec rule in `CLAUDE.md`). Geist and Geist Mono are open-licensed.

```css
--font-sans: "Geist", ui-sans-serif, system-ui, sans-serif;
--font-mono: "Geist Mono", ui-monospace, monospace;
```

| Use | Size | Weight | Notes |
| --- | --- | --- | --- |
| Page title (h1) | 28px | 600 | letter-spacing -0.02em |
| Section title (h2) | 15px | 600 | card and panel headers |
| Body, table cells | 14px | 400 | primary text |
| Nav items, buttons | 14px | 500 | |
| Labels, table headers | 13px | 500 | `--muted-foreground` |
| Captions, meta | 12px | 400 | `--muted-foreground` |
| Stat values | 26px | 500 | Geist Mono |
| Timers, ISK, counts | inherits | 400–500 | Geist Mono |

## Spacing and layout

A 4px base. Common steps: 4, 8, 12, 16, 20, 24, 32.

| Element | Value |
| --- | --- |
| Sidebar width | 256px, fixed, right border |
| Top bar height | 64px, bottom border |
| Content padding | 28px top and bottom, 32px sides |
| Gap between page sections | 24px |
| Gap between cards | 16px |
| Card padding | 20px |
| Right rail (detail panels) | 340px |
| Desktop design width | 1440px; layouts must hold down to 1280px |

Page structure: sidebar → top bar with breadcrumb, search and icon buttons → page header (title, one-line description, actions on the right) → optional stat row → main content with an optional right rail.

## Components

**Buttons**, 36px tall (32px inside tables), `--radius-sm`, 14px/500, 16px horizontal padding.
- Primary: `--primary` fill, `--primary-foreground` text. At most one per screen region.
- Outline: `--background` fill, `--border` border, `--foreground` text. The default for secondary actions.
- Destructive: outline style with `--destructive` text; filled only inside confirmation dialogs.
- Icon-only: 36×36 outline button with an `aria-label`.

**Inputs**, 36px tall, `--background` fill, `--input` border, `--radius-sm`. Search inputs carry a 16px leading icon. Every input has a `<label>`, visually hidden if the design omits it.

**Cards and panels**: `--card` fill, 1px `--border`, `--radius-lg`, 20px padding. Titles are h2 at 15px/600.

**Stat cards**: label (13px muted), value (26px Geist Mono), caption (12px muted). Four across on desktop.

**Tables**: live inside a card. Header row 13px/500 muted, left-aligned; body 14px; 16px vertical cell padding; 20px horizontal padding on the outer columns, 12px on inner ones; 1px `--border` between rows; row actions right-aligned as small outline buttons. Primary cell: name at 14px/500 with a 12px muted sub-line. Sorting and filtering happen on the server.

**Tabs**: a segmented control. `--muted` track with 4px padding and `--radius`; the active tab is a `--background` pill with `--foreground` text; inactive tabs are muted text on the track.

**Badges**: 12px, 2px × 8px padding, `--radius-sm`.
- Neutral (tags, ore types): 1px `--faint` outline, `--foreground-soft` text.
- Status: soft fill (`--accent-soft`, `--info-soft`, `--destructive-soft`) with matching text and a 6px leading dot.
- Notification levels use the status badges: danger is destructive, warning is accent, success is info (healthy), and info is the neutral badge with a dot. The top bar's bell carries the unread count as an accent count pill.

**Sidebar navigation**: grouped under 12px/500 muted headings. Items are 36px tall links with a 16px icon, 14px text, `--radius-sm`. Active item: `--accent-surface` fill, `--foreground` text, weight 500, `aria-current="page"`. Count pills use the accent fill with dark text. The sidebar is as tall as the window and stays put while the page scrolls; its links scroll inside it if they must. The signed-in character sits at the bottom above a top border, and opens the account menu: a small panel above it (the browser's own popover: no script, Escape or a click outside closes it) with Services, Token Management, Access tokens and Log out. Those live only there, never in the sidebar or elsewhere. Admins arrange the sections, items, folders (a collapsible item, open while one of its pages is shown) and custom links on the Menu page; the default is Account, Fleet, Industry, Corporation, Apps and Admin, and a section with nothing this person may open isn't shown. Apps' links go in the section their manifest names (Fleet for Fleet Activity Tracking, Ship Replacement and Structure Timers; Industry for Moon Mining and Structures; Corporation for Member Audit and HR Applications), or Apps. Admin holds one item, Administration (see below); each admin page starts hidden in the sidebar, and admins can pin any of them on the Menu page.

**Watermark**: on pages showing sensitive data, a single line in 11px Geist Mono, `--faint` color, bottom-right of the data card: `Viewing as <character> · <EVE time>`. Decorative, `aria-hidden`.

**Icons**: Lucide-style outline icons, 16px, stroke width 2, `currentColor`. Bundled as inline SVG or a sprite, never fetched. No emoji.

## Administration

Admin pages live in one place instead of filling the sidebar. The sidebar's Administration item opens an overview (`/admin`) with every admin page the viewer may open, grouped: **Access** (States, Groups, Auto Groups, Permissions, Permissions Audit), **Members** (Users, Blacklist, Compliance Report, Corporation Stats), **Integrations** (Discord, Fleet Pings, Apps) and **Instance** (System, Menu, Audit log, Setup). A group with nothing the viewer may open is left out. The list lives in `crates/web/src/admin_nav.rs`; a new admin page goes there, with a group and one sentence on what it does.

- **Overview**: page header, then each group as a 15px/600 heading with a one-line muted description, over tiles two across (three from 1536px). A tile is a link: a 32px `--muted` square holding the page's icon, its name at 14px/500, and its sentence at 13px muted. Hover lightens it like any surface.
- **Rail**: every admin page (not the overview) shows a 208px second column right of the sidebar, cascading out of it: the same fill, a right border, its own 64px header ("Administration") in line with the top bar, then an Overview link and the groups under nav headings, each page a 32px link. The current page is marked like the sidebar's. Like the sidebar it is window-tall and stays put while the page scrolls. It shows from 1280px; narrower, the sidebar item and the overview are the way around.
- The sidebar's Administration item stays marked on every page in the hub.

## Confirm it's you

Sudo mode's interstitial (`/reauthenticate`), shown when an owner-only or sensitive action needs a fresh EVE login. It uses the bare layout of the login page (no sidebar): one card, 400px wide.

- **Header**: h2 "Confirm it's you", then "Log in with EVE again to continue: **<action>**", the action named in plain words ("Uninstall an app"), in `--foreground` at 500.
- **Body**: one muted 14px paragraph: the login must be from the last 15 minutes, with which character (the account's main, by name), and that the admin comes back to the page they were on to submit again. Nothing is sent for them.
- **Actions**: the primary full-width button "Log in with EVE Online" (a form post), and under it a full-width outline "Cancel" back to that page.
- No countdowns, warnings or accent: it's a routine step, not an error.

## Motion

Motion says something arrived or is on its way; it never decorates. Everything moves for 200ms or less (the progress line, which grows for as long as a request takes, aside), eases out, and uses opacity and at most a 4px rise: no scaling, bouncing, sliding panels or parallax. Under `prefers-reduced-motion: reduce`, none of it happens.

- **Page content** fades in and rises 4px over 180ms when a page arrives. The sidebar, top bar and rail stay still. The overview's tiles follow each other 25ms apart, at most 100ms.
- **Swapped content** (htmx fragments) fades in over 150ms.
- **Progress**: while a request runs, a 2px `--muted-foreground` line grows across the top of the page, appearing only after 150ms so quick requests show nothing. It is neutral, not the accent.
- **Folders** in the sidebar open and close over 150ms where the browser can animate to auto height, and snap elsewhere.
- **Hover and state changes**: 150ms color and opacity transitions, as before.
- Counts and countdowns never animate between values: they change in place (Geist Mono keeps them from jittering).

## Data display

- **Relative times** for recent events ("1h 12m ago") with the absolute EVE time in a tooltip or sub-line.
- **Countdowns** in Geist Mono, updated live from the server (server-sent events); the nearest one gets the accent color. A countdown to an instant that is already known (a skill finishing, a timer) ticks in the browser instead, from the bundled `assets/live.js`, with the absolute EVE time in its tooltip: `2d 4h 13m` over a day, `4h 13m` over an hour, `13m 05s` under it. Once the instant passes it reads `done`.
- **EVE time (UTC)** everywhere, labeled "EVE".
- **ISK** abbreviated in tables (`1.24b`, `350.2m`), full value on hover or in detail views.
- **EVE names** (systems, moons, structures, characters) exactly as ESI returns them. Character portraits and corp or alliance logos come from CCP's image server at 32px in tables and 36px in the sidebar (20px as an app's entity values, 64px in a profile), with initials as the fallback.

**State badges** show an account's access state as a status badge: Member in the accent, Blue in info, Guest and admin-made states neutral. The label is always the state's name.

## Configuration pages

Admin settings must make sense without documentation open. Alliance Auth's settings didn't; ours do. Every configuration page follows these rules on top of the rest of this file.

- **Plain language, in terms of pilots.** Each setting carries one sentence on its effect ("Pilots whose main is in one of these get Member"), never internal names. Headings name the thing, not the table.
- **Ordered lists are cards in order.** When order matters (states), each item is a card, top first, with up and down icon buttons, and one sentence says how the order is used ("The highest match wins").
- **EVE entities are chips.** An alliance, corporation or character in a list is a chip: 20px logo or portrait, name, kind in muted text, and a remove icon button with an `aria-label`. They are added with an exact-name search whose results show the logo and kind before anything is added.
- **Live counts.** Next to each item, how many accounts it covers right now, in Geist Mono.
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

Plugins never ship their own styles. They return a declarative page description (headers, stat rows, tables, cards, tabs, forms, badges, profiles, row actions, text to copy) and the host renders it with these components, so every plugin looks native. Any exception needs a documented reason and still uses these tokens.

- **Page header**: the title, the one-line description under it, and on the right the page's own links (sub-pages such as "Skill Sets · Character Finder · Reports") as a segmented control like Tabs, the current page marked, then at most one primary button that opens a page ("Create timer"). Links to sub-pages live here, never in "More" cards at the bottom.
- **Entities** (a character, corporation, alliance, faction, or an item or ship type) are their 20px picture from CCP's image server, `--radius-sm`, then the name at 14px: portraits for characters, logos for corporations, alliances and factions, icons for types (CCP's 32px icon). The host builds the image address from the kind and id; a plugin never supplies a URL. No id (0 or less) gets initials on `--muted`, as avatars do. In tables the picture sits before the name.
- **Row actions**: buttons that post (Approve, Reject, Close) sit in a table's cell as small 32px buttons, right-aligned, several side by side: outline by default, outline with `--destructive` text for destructive ones, primary only for a region's one main action. A destructive or far-reaching one asks first: the browser's own popover, centred over a dimmed page, 360px wide, `--card` fill, 20px padding, with one sentence stating the consequence ("Its 4 members lose access"), then Cancel (outline) and the action, filled destructive or primary. No script; Escape or a click outside cancels.
- **Text to copy** (a fitting in EFT format, a list): a card with its title on the left of the header and a small outline Copy button on the right; the text in Geist Mono at 13px, exactly as given (spaces and line breaks kept), on `--background` with a 1px border, `--radius-sm`, 12px padding, scrolling past 384px. The button reads "Copied" for 1.5s after copying; where the browser can't copy, it selects the text instead.
- **Profile**: the top of a page about one character (or corporation). A card with the 64px portrait or logo on the left (`--radius`), then the name as an h2 at 20px/600 with an optional muted 13px subtitle, and on one line under it the corporation and alliance as 20px entities in `--foreground-soft`. Badges (neutral or status) follow the name. Under them, the facts: a grid of three columns (two below 1280px, four from 1536px), each a 12px muted label over its 14px value; numbers, ISK, times and countdowns in Geist Mono. It replaces a tall label/value card as a page's overview.
- **Progress**: a 4px bar, `--muted` track, `--foreground-soft` fill, `--radius-sm`, full width of its cell, with an optional 12px muted label and the percentage in Geist Mono above it. A bar between two known instants (a skill in training) fills live in the browser. Neutral, never the accent; no animation between values (it moves as time does).
- **Live pages**: a page that says it is still filling in (a first sync) reloads its content in place every few seconds (5 to 300) while it says so, and stops once it doesn't. The content swaps without the fade and without scrolling, so the page doesn't blink; a page with a form never reloads under someone typing, and an audited page never reloads at all.

**Owners** (AA's Add Owner). For an app that reads corporation data through data sources, the host adds two things around the app's own content; the app never sees either.

- **Add owner**: an outline button with a plus icon at the right of the page header, on every page of the app. It shows for those who may offer a character: holders of the app's `manage` permission or of one of its `add_…` permissions, and the admins who approve sources, as long as they may open the app's main page. It is one EVE login, which comes back to that page.
- **Owners card**: on the app's main page, after the app's content. Its header lists the scopes asked for, as outline badges. The table shows the character (portrait), the corporation (20px logo), who added it and when, and the status: approved, waiting for an admin, or changed corporation. Admins who approve sources see every source, with Approve and Remove as row buttons, and under the table the sources withdrawn or removed in the last 30 days. Everyone else sees only their own characters, with Withdraw. The card isn't shown to a viewer who can't offer or approve and has nothing offered.
- **Token Management** lists the account's own owners in every app (an "App owners" card: character, app, status, Withdraw), so a pilot can always see and withdraw them, even without access to the app any more.

## Don't

- No drop shadows, gradients, glows or glassmorphism.
- No more than one accent color per screen.
- No Google Fonts, CDNs or remote assets at runtime.
- No emoji in the UI.
- No animation longer than 200ms except the progress line, and none that ignores reduced motion.
- No zebra-striped tables or heavy table borders.
- No light-gray text below 4.5:1 contrast for anything a user needs to read.
