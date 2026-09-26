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

**Sidebar navigation**: grouped under 12px/500 muted headings. Items are 36px tall links with a 16px icon, 14px text, `--radius-sm`. Active item: `--accent-surface` fill, `--foreground` text, weight 500, `aria-current="page"`. Count pills use the accent fill with dark text. The sidebar is as tall as the window and stays put while the page scrolls; its links scroll inside it if they must. The signed-in character sits at the bottom above a top border, and opens the account menu: a small panel above it (the browser's own popover: no script, Escape or a click outside closes it) with Services, Token Management, Access tokens and Log out. Those live only there, never in the sidebar or elsewhere. Admins arrange the sections, items, folders (a collapsible item, open while one of its pages is shown) and custom links on the Menu page; the default is Account, Fleet, Apps and Admin. Admin holds one item, Administration (see below); each admin page starts hidden in the sidebar, and admins can pin any of them on the Menu page.

**Watermark**: on pages showing sensitive data, a single line in 11px Geist Mono, `--faint` color, bottom-right of the data card: `Viewing as <character> · <EVE time>`. Decorative, `aria-hidden`.

**Icons**: Lucide-style outline icons, 16px, stroke width 2, `currentColor`. Bundled as inline SVG or a sprite, never fetched. No emoji.

## Administration

Admin pages live in one place instead of filling the sidebar. The sidebar's Administration item opens an overview (`/admin`) with every admin page the viewer may open, grouped: **Access** (States, Groups, Auto Groups, Permissions, Permissions Audit), **Members** (Users, Blacklist, Compliance Report, Corporation Stats), **Integrations** (Discord, Fleet Pings, Apps) and **Instance** (System, Menu, Audit log, Setup). A group with nothing the viewer may open is left out. The list lives in `crates/web/src/admin_nav.rs`; a new admin page goes there, with a group and one sentence on what it does.

- **Overview**: page header, then each group as a 15px/600 heading with a one-line muted description, over tiles two across (three from 1536px). A tile is a link: a 32px `--muted` square holding the page's icon, its name at 14px/500, and its sentence at 13px muted. Hover lightens it like any surface.
- **Rail**: every admin page (not the overview) shows a 208px second column right of the sidebar, cascading out of it: the same fill, a right border, its own 64px header ("Administration") in line with the top bar, then an Overview link and the groups under nav headings, each page a 32px link. The current page is marked like the sidebar's. Like the sidebar it is window-tall and stays put while the page scrolls. It shows from 1280px; narrower, the sidebar item and the overview are the way around.
- The sidebar's Administration item stays marked on every page in the hub.

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
- **Countdowns** in Geist Mono, updated live from the server (server-sent events); the nearest one gets the accent color.
- **EVE time (UTC)** everywhere, labeled "EVE".
- **ISK** abbreviated in tables (`1.24b`, `350.2m`), full value on hover or in detail views.
- **EVE names** (systems, moons, structures, characters) exactly as ESI returns them. Character portraits and corp or alliance logos come from CCP's image server at 32px in tables and 36px in the sidebar, with initials as the fallback.

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

Plugins never ship their own styles. They return a declarative page description (headers, stat rows, tables, cards, tabs, forms, badges) and the host renders it with these components, so every plugin looks native. Any exception needs a documented reason and still uses these tokens.

## Don't

- No drop shadows, gradients, glows or glassmorphism.
- No more than one accent color per screen.
- No Google Fonts, CDNs or remote assets at runtime.
- No emoji in the UI.
- No animation longer than 200ms except the progress line, and none that ignores reduced motion.
- No zebra-striped tables or heavy table borders.
- No light-gray text below 4.5:1 contrast for anything a user needs to read.
