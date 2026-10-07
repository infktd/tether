# Writing a Tether plugin

This is the guide for anyone (person or AI agent) building a plugin with `tether-plugin-sdk`. The machine-readable contract is `wit/plugin.wit` in the Tether repository; this file explains how to use it.

The SDK and the app interface (`wit/plugin.wit`) are licensed under MIT or Apache-2.0, at your option, so your app can use any license you choose. Tether itself is GPL-2.0-or-later.

## What a plugin is

A Rust library compiled to a WebAssembly component. Tether loads it at runtime, with no restart, and calls it inside a sandbox. A plugin:

- describes pages as data; the host draws them with its own templates, so every plugin looks native and no plugin code runs in a browser;
- can only call what the host API offers (see "What the host offers" below);
- never sees an ESI token, a secret, another plugin's data or Tether's own tables.

## Minimal plugin

`Cargo.toml`:

```toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
# Pin to a release tag once Tether publishes them.
tether-plugin-sdk = { git = "https://github.com/infktd/tether" }
```

`src/lib.rs`:

```rust
use tether_plugin_sdk::{Page, PageError, Plugin, Request, log};

struct MyPlugin;

impl Plugin for MyPlugin {
    fn render(request: Request) -> Result<Page, PageError> {
        match request.path.as_str() {
            "" => Ok(Page::new("My plugin").text("Hello.")),
            _ => Err(PageError::NotFound),
        }
    }
}

tether_plugin_sdk::export!(MyPlugin);
```

Build it:

```bash
rustup target add wasm32-wasip2
cargo build --target wasm32-wasip2 --release
# target/wasm32-wasip2/release/my_plugin.wasm is the component
```

No other tooling is needed: `wasm32-wasip2` produces a component directly. Forgetting `export!` only fails when linking the `.wasm`, so always build for `wasm32-wasip2` (and run `cargo clippy --target wasm32-wasip2`), not just for your own machine.

`examples/hello-plugin` in the repository is a complete example with every kind of section and value: a profile, a card grid, entities, countdowns, progress bars, row actions, text to copy, a link to share, page links and a live page.

## plugin.toml

Every package carries a `plugin.toml`. Unknown fields are refused, so a typo fails loudly instead of silently dropping a capability.

```toml
[plugin]
id = "acme.mining-ledger"   # 3-50 lowercase letters, digits and single . - _, starting with a letter
name = "Mining ledger"     # up to 60 characters
version = "0.3.1"          # MAJOR.MINOR.PATCH, no leading zeros
host_api = "1"
description = "Moon mining for the corp"                  # optional, up to 300 characters
icon = "moon"              # optional: its sidebar and header icon, one of Tether's (see The app shell)
repository = "https://github.com/example/mining-ledger"   # optional

[publisher]
key = "RWQ..."             # the second line of your minisign .pub file; required (only the apps built into Tether's own image go without)

[capabilities]             # all optional; ask only for what you use
storage = true
discord = ["send_message"]
http = ["janice.e-351.com"]         # exact HTTPS hostnames, at most 10
groups = true                       # which groups each viewer is in (see Who's looking)
downloads = true                    # CSV files to download (see Downloads)
notify = true                       # notices in the bell (see Notices)

[capabilities.esi]
user = ["esi-wallet.read_character_wallet.v1"]              # pilots register characters for your app with these
data_source = ["esi-industry.read_corporation_mining.v1"]  # characters people with your add_… permissions add as data sources
# owner_permissions = ["manage_contacts"]                 # which permissions add data sources (default: your add_… ones)

[[capabilities.schedules]]
name = "sync_mining"
every = "30m"              # 5m to 7d: m, h or d

[capabilities.secrets.janice_api_key]   # at most 10
host = "janice.e-351.com"  # one of `http`
header = "X-ApiKey"        # the header it's sent in
# prefix = "Bearer "       # optional, put before the value

[permissions]              # granted like core ones, as plugin.<id>.<name>
view = "View the mining ledger"
manage = "Manage the mining ledger"

[permission_notes]         # optional: who each is usually for, under it on Permissions (at most 400 characters)
view = "Usually Member: everyone who mines for the corporation."
manage = "For directors who run the ledger: holders change its settings."

[renamed_permissions]      # optional: what an earlier version called them (see Packaging)
# see = "view"             # old name = new name; the old one's grants move on upgrade
```

The admin sees every capability before approving an install. Secrets are values like API keys that the admin enters. Each goes to one declared host in one header (not `Cookie`, `Host` or headers that frame the request); the host adds it to your requests there, and your plugin never sees it (see HTTP below). `http` can't name Tether's own destinations (ESI, EVE SSO, CCP's image server, Discord, GitHub or their subdomains): a package that does is refused, since ESI and Discord go through the host API. Names and descriptions can't contain control characters or invisible formatting (bidi overrides, zero-width characters).

## Packaging and signing

A package is a `.zip` with exactly these entries (anything else, symlinks, encryption, repeated names or an archive comment gets it refused):

| Entry | Limit |
| --- | --- |
| `plugin.toml` | 64 KiB |
| `plugin.wasm` | 32 MiB |
| `migrations/0001_<name>.sql`, `0002_...`, with no gaps; `<name>` is lowercase letters, digits and `_` | 100 files, 256 KiB each |
| `ui/<name>.png`, `.jpg`, `.jpeg`, `.webp` or `.gif` (really that type; no SVG) | 32 files, 1 MiB each |
| `rotation.txt` and `rotation.txt.minisig`, only when changing keys | 4 KiB each |

The whole package is at most 40 MiB. Compress with deflate or store uncompressed.

Sign the finished zip with [minisign](https://jedisct1.github.io/minisign/) and ship the signature next to it:

```bash
minisign -G -p tether.pub -s tether.key     # once; put the .pub's key line in plugin.toml
zip -X -r my-plugin.zip plugin.toml plugin.wasm migrations ui
minisign -S -s tether.key -m my-plugin.zip  # writes my-plugin.zip.minisig
```

The first install pins your key for your plugin id. Every later version must be signed with the same key. To move to a new key, put the new key in `plugin.toml`, sign the package with the new key, and include a statement signed with the old key:

```bash
printf 'tether-key-rotation v1\nplugin: %s\nold: %s\nnew: %s\n' acme.mining-ledger "$OLD_KEY" "$NEW_KEY" > rotation.txt
minisign -S -s old.key -m rotation.txt      # writes rotation.txt.minisig
```

Keep the rotation files in later packages or drop them; either works once installs have moved to the new key. If you lose your key, admins have to re-pin it by hand, so keep a backup.

### Publishing on GitHub

Admins can install straight from a repository, and Tether looks there once a day for newer versions. Attach the package and its signature to a GitHub release as `<plugin id>-<version>.zip` and `<plugin id>-<version>.zip.minisig`, for example `acme.mining-ledger-1.2.0.zip`. The version in the name must match `plugin.version`. Drafts and pre-releases are skipped. Tether reads the 30 newest releases and takes the highest version of your plugin that has both files, so one repository can publish several plugins. The admin then enters the plugin id when installing.

To try a build before publishing it, run a development build of Tether (`cargo run -p tether-server --features dev`): its Apps page can also install a signed package from a file. Release builds install only from GitHub. A package under the id of an app that comes with Tether (such as `tether.moon-mining`) is always refused.

An upgrade is reviewed like an install. The admin sees what the new version asks for beyond the old one: hosts, secrets, scopes, timers, filters and permissions. A version must keep every migration already applied unchanged, and keep `storage` once it has data. A permission you drop takes its grants with it; renaming one is a drop plus an add, unless you say so in `[renamed_permissions]` (old name = new name, e.g. `view = "extractions_access"`): then the old one's grants move to the new one. The old name must be gone from `[permissions]` and the new one declared, and a grant moves only within your app and only onto a new name nobody held before, so no grant is merged into one someone already has; nor can a rename create a `manage` or `add_...` permission (they let accounts offer your app data sources) from one that wasn't. The admin's review shows each rename and how many grants move, and the moves are in the audit log. Rolling back moves them back. Admins can roll back one version: your earlier package goes back, and if your new migrations ran, so does the data (from the snapshot taken before them).

## Who sees what

A plugin's pages live at `/plugins/<id>/<path>`, for signed-in users only. `plugin.toml` says who may open which:

```toml
[permissions]
view = "See the mining ledger"
manage = "Change ledger settings"

[[pages]]              # the longest matching path wins
path = ""              # every page...
permission = "view"
[[pages]]
path = "settings"      # ...except settings/...
permission = "manage"
[[pages]]
path = "mail"          # mail/... needs view too,
permission = "view"
audit = true           # and every view of it is in Tether's audit log
[[pages]]
path = "reports"       # reports/... opens for view or manage,
permission = ["view", "manage"]   # any one of them
[[pages]]
path = "apply"         # apply/... is for anyone signed in with a main,
signed_in = true       # no permission (AA's login_required views)

[[navigation]]         # sidebar links, shown to whoever may open them
label = "Mining"
path = ""
section = "industry"   # optional: account, fleet, industry, corporation, apps (the default) or admin

[[views]]              # the app shell: your views bar, in order; the first is the main page
label = "Overview"
path = ""
[[views]]
label = "Reports"
path = "reports"

[action]               # one primary action, the header's button on every page but its own
label = "New ledger"
path = "add"

[[manage]]             # pages for those who run the app, after Settings; Manage opens the first
label = "Ore types"
path = "ores"
```

- Permissions are granted like Tether's own, to states and groups, as `plugin.<id>.<name>`.
- A `[[navigation]]` link goes in its `section` of the sidebar: `account`, `fleet`, `industry`, `corporation`, `apps` or `admin`. Leave it out for `apps`; any other name gets the package refused. It's only where the link starts out: admins can move, rename or hide it on the Menu page, and a section with nothing in it isn't shown.
- A page no `[[pages]]` rule covers is for admins only (`admin.plugins`), never for everyone. Declare a rule for every page people should see.
- A rule names a `permission`, or a list of 1 to 5 of them any one of which opens its pages (as aa-afat's Logs open for `log_view` or `manage_afat`), or says `signed_in = true`, never both. Every link Tether draws to those pages (sidebar, views, Manage, the action, the ⌘K palette) follows the rule; admins see each rule's permissions at install, and an upgrade that adds one to a rule asks them again. `signed_in` pages open to every account signed in with a main, Guests included and the Blacklist not (it holds only what an admin grants it): use it only where AA asks no more than a login (applying to a corporation), and gate what they show and do with `viewer.can(...)` as usual. Admins see such rules at install as "any signed-in pilot", and an upgrade that opens a page asks them again.
- `audit = true` on a rule writes every view of a page under it (opened, reloaded, or drawn for a form post) to Tether's audit log as `plugin.page_view`, with who, the path and the query, before your plugin is called. Use it for pages showing someone else's private data, such as their mail. A view that can't be recorded isn't shown.
- Someone who may not open a page gets the same "nothing here" as for a page that doesn't exist; your plugin isn't called.
- Paths are link paths (see below). The query string is capped at 2 KiB and 20 pairs; `_tab` is the host's (which tab is showing) and never reaches you. Each person can open 120 of a plugin's pages a minute.

### The app shell

Tether draws every app's frame, the same for all of them: the page header (your icon, your app's name over the page's title, its description), the views bar, the primary action and Manage. You declare them in `plugin.toml`; your pages return only their content.

- `icon` (in `[plugin]`): one of `activity`, `blueprint`, `book`, `box`, `chart`, `citadel`, `clipboard`, `clock`, `contract`, `crosshair`, `flag`, `globe`, `hexagon`, `life-buoy`, `megaphone`, `moon`, `package`, `pin`, `radio`, `scan-user`, `scroll`, `shield`, `users`. Without one, a generic package.
- `[[views]]`: your views bar, in order: at most 8, labels up to 30 characters, in sentence case. The first is your main page (`path = ""`), usually "Overview". Required of every app with pages to open (any `[[pages]]` rule or `[[navigation]]` entry): Tether refuses the package without it. One view alone draws no bar. A page's own `.link()`s are its record's sub-pages (chips under the bar), never your views.
- `[action]`: your one primary action ("New ledger"), a button in the header on every page but its own. A page's own `.button(...)` (a record's Edit) takes its place on that page. Name actions that create something "New ...".
- `[[manage]]`: pages for those who run the app, after Settings (which Tether adds when a `[[pages]]` rule covers `settings`): the header's Manage button opens the first, and the bar on those pages lists them all. Not your main page, and nothing under `settings`. On a Manage page the bar shows the Manage pages and the eyebrow reads "<App> · Manage". Tether adds two of its own after yours: Data sources (an app with data-source scopes: each source, how it's doing, Add data source, and for app admins which member corporations you can read) and Activity (for app admins: your ESI and HTTPS calls, schedules with Run now, jobs and log). Their paths, `data-sources` and `activity` and everything under them (in any case), are Tether's in every app: a package with a page rule, sidebar entry, view, Manage page or action there, or any of those labelled "Data sources" or "Activity", is refused, and your main page rule never reaches them.
- Each entry shows only to whoever may open its page under your `[[pages]]` rules, so nobody sees a link they can't follow: give a view or action meant for fewer people its own rule, naming every permission that opens it (`permission = ["add_fatlink", "manage_afat"]`).

## Pages

`render` gets a `Request` (the path below the plugin's pages, and the query string) and returns a `Page` or a `PageError`.

- `PageError::NotFound` and `PageError::Forbidden` show the usual pages; `PageError::Failed(text)` shows a generic error to the user, and `text` to admins in the plugin's log.
- A page has a title, an optional one-line description, links beside the title, sections, and optional tabs (each with its own sections).
- Tables longer than 25 rows are paged by Tether (Previous / Next, the page in the address): send them whole, up to your own sensible limit.
- Lists get Tether's toolbar over them (DESIGN.md, Toolbar): a search box once a table has 8 rows or more, its words in the address as `q`, finding rows among those your page shows (every page of them); your tabs as view chips beside it. Don't draw a search form of your own. If your page should search its own data instead (rows it doesn't show, or by what it doesn't show), say so with `.toolbar(Toolbar::new().search("Search moons, systems, refineries"))` and read `request.search()` in `render`: Tether then hides none of your rows. Filters go there too: `.filter("rarity", "Rarity", vec![("32".into(), "R32".into()), ...])` (at most 8, each a query parameter of yours, not `q`), drawn as chips with a × to take one off and "+ Filter" for the rest; read the chosen value with `request.param("rarity")`. A filter taking several values at once, showing what has any of them (tags), is `.filter_any("tag", "Tag", choices)`, read with `request.params("tag")` (its values can't hold commas: the address joins them with them).
- A row's name links to its record (no separate Open or Details column): to its own page (`link(name, "moon/40161234")`; a pilot's or a corporation's, with its picture, `character(id, name).link(format!("character/{id}"))`), or to a record panel beside the list (DESIGN.md, Record panel) with a query to the same page (`link(name, "moons?moon=40161234")`): Tether adds it to the page's address, keeping the search, filters and tab. While `request.param("moon")` names one, give the page `.panel(RecordPanel::new("moon", "Moon · R32", name).context(...).figure(parts, "1.84B").fact(...).open("Open moon", "moon/40161234").action(...))`: Tether draws it beside the list, marks its row, and Close takes the parameter out again. Its action posts like any row action on the page. The parameter is the viewer's to type: draw a panel only for a record they may see, and in `submit` act on the action's own fields (checked again), not on `request.param`.
- `identity::acting()` is the character the pilot is using: the one they chose in their account menu (Change character), else their main. Use it where you act for them (whose FAT to tick, whose fits, a request's pilot). `viewer.main` stays their real main: scope what they may see by it and by their permissions, never by `acting()`, or an alt in another corporation would open that corporation's data.
- Settings: an app configured by admins puts that at `settings` (and pages under it, `settings/...`). Tether opens it from the app's Administration page (a Settings button) and leaves links to it out of the app's header, except on the settings pages themselves, so they can link to each other.
- Sections: a row of stats (`stats`, at most 8), a `table`, a `card` of label/value fields, a paragraph of `text`, a `form`, a `profile` (the top of a page about one character or corporation), `cards` (a grid of compact profiles, such as My Characters, each opening one of your pages), or `code` (text to copy).
- Tether lays sections out, in your order: titled tables that follow each other with the same columns (four or more) are drawn as one table, each title a heading row, so their columns line up (give a fit's slots, drones and cargo the same columns); and narrow sections of a kind that follow each other sit side by side on wide screens (code blocks together; tables of up to three columns together). To keep two such tables apart, put another section between them or name a column differently.
- A card grid (`CardGrid::new().linked(profile, "character/90000001")`, or `.card(profile)` for one that opens nothing) can ask with `.register()` to start with Tether's own Register Character card, which opens registering characters for your app. Only apps with user scopes get it: registering is how their characters arrive. A grid with no items and `.register()` draws just the card: the way to send a pilot to register from a page that has no characters yet.
- Values are typed so the host formats them consistently:
  - `Value::Text`, `Value::Number` (counts, IDs), `isk(amount)` (abbreviated in tables), `time(rfc3339)` (EVE time), `badge(label, tone)`, and `link(label, path)` to another page of the same plugin (`.primary()` draws it as a button; the path may carry a query, as a redirect's, which joins the page's own when it's the page shown);
  - entities: `character(id, name)`, `corporation(...)`, `alliance(...)`, `faction(...)` and `item_type(...)` (items and ships), drawn as the 20px portrait, logo or icon from CCP's image server and the name. Tether builds the image address from the kind and id; you never give a URL. An id of 0 or less gets initials;
  - `countdown(rfc3339)`: the time left ("2d 4h 13m"), ticking in the browser, the EVE time on hover, "done" once it's passed;
  - `progress(fraction)`: a thin bar, 0 to 1, with `.label(...)`; `.between(from, to)` (two RFC 3339 instants) makes it fill live, e.g. for a skill in training;
  - `action(label, form)` and `actions(vec![...])` (at most 4 side by side): buttons that post, for row actions (see Forms);
  - `add_owner(label)`: Tether's own Add data source button with your words ("Log in with the fleet boss"), for apps with data-source scopes, drawn only for those who may add data sources, with Tether's own line and the scopes beside it; the login comes back to the page with `owner=<character id>` in the query. Anyone can type a query, so treat `owner` as a hint: use it only if it's one of the viewer's characters and in `esi::data_sources()`, and check the choice again when the form is posted;
  - `share(path)`: one of your pages as its full address, to paste outside Tether (a register link in fleet chat), read-only with a Copy button. You give a link path; Tether writes the site's address before it (you never learn it, and can't give any other address).
- Use `Tone::Accent` for the single most important thing on a screen, and nothing else.
- Your app's views, its primary action and its Manage pages are Tether's to draw, from `plugin.toml` (see The app shell): don't add links to them. `.link(label, path)` adds a record's own pages, drawn as chips under the views bar (a character's Skills · Assets · Wallet; at most 8); the page shown is marked. `.button(label, path)` is the page's own primary action (a record's Edit), shown in the header instead of the app's `[action]` on that page.
- Live pages: `.refresh(seconds)` (5 to 300; anything else is brought into that range) makes Tether reload the page's content in place at that interval, for as long as your render keeps asking: say "Syncing..." and fill in as a job stores data, then leave it out. Pages with a form never reload (someone may be typing). Each reload is a page view (the 120 a minute count).

Builders keep this short:

```rust
use tether_plugin_sdk::{
    CodeBlock, Column, Profile, Stat, Table, Tone, action, actions, badge, character, corporation,
    alliance, countdown, isk, item_type, progress, time,
};

let table = Table::new(vec![Column::text("Moon"), Column::text("Owner"), Column::numeric("Value"), Column::numeric("Pops in"), Column::numeric("")])
    .title("Extractions")
    .empty("No extractions yet.")
    .row(vec![
        "1DQ1-A I - Moon 1".into(),
        corporation(98_000_001, "Example Corp").into(),
        isk(1.24e9),
        countdown("2026-09-24T18:00:00Z"),
        actions(vec![
            action("Fracture", "moon").field("moon", "40161234"),
            action("Cancel", "moon").field("moon", "40161234").field("cancel", "yes")
                .tone(Tone::Danger)
                .confirm("The extraction stops and its ore is lost."),
        ]),
    ]);

Page::new("Moons")
    .description("Extractions from our structures")
    .profile(
        Profile::new(character(90_000_001, "Example Pilot"))
            .subtitle("Main of 3 characters")
            .corporation(corporation(98_000_001, "Example Corp"))
            .alliance(alliance(99_000_001, "Example Alliance"))
            .badge(badge("Registered", Tone::Success))
            .fact("Skill points", 48_210_332)
            .fact("Ship", item_type(587, "Rifter"))
            .fact("Training", progress(0.0).between("2026-09-24T18:00:00Z", "2026-09-25T02:00:00Z").label("Gunnery V")),
    )
    .stats(vec![Stat::new("Ready", badge("3", Tone::Accent))])
    .table(table)
    .code(CodeBlock::new("[Rifter, Example]\nDamage Control II\n").title("Doctrine fit").copy_label("Copy fit"))
```

Instruments and timelines (EVE's own shapes; the host draws them in Tether's style):

```rust
use tether_plugin_sdk::{Lane, LaneItem, Timeline, Tone, composition, composition_large, defenses, levels, part};

// A skill in the queue: four levels trained, the fifth in training.
let level = levels(4, Some(5));
// A moon's ores as a small ring (grade 0 = R4 ... 4 = R64), or large with its value.
let ores = composition(vec![part("Xenotime", 0.31, 4), part("Sylvite", 0.69, 0)]);
let big = composition_large(vec![part("Chromite", 0.34, 2), part("Bitumens", 0.66, 0)], "1.84B");
// A structure in its armor timer: shield gone, core pulsing.
let hull = defenses(0.0, 1.0, 1.0, true);
// The next days on lanes, prime time shaded, a proposal dashed.
let week = Timeline::new("2026-09-27T00:00:00Z", "2026-10-01T00:00:00Z")
    .title("The next days")
    .window("2026-09-27T18:00:00Z", "2026-09-27T21:00:00Z")
    .lane(
        Lane::new("Fleets")
            .item(LaneItem::new("Stratop", "2026-09-28T07:00:00Z").until("2026-09-28T10:00:00Z").tone(Tone::Warning).link("op/7"))
            .item(LaneItem::new("Start the drill", "2026-09-29T19:00:00Z").planned()),
    );
page = page.timeline(week);
```

The host refuses a page (and logs why, for admins) if it breaks these rules:

| Rule | Limit |
| --- | --- |
| Title | not empty |
| Sections, counting those in tabs | 40 |
| Tabs | 10 |
| Stats in a row | 8 |
| Table columns | 1 to 20; every row has exactly one value per column |
| Table rows | 500 (paginate with links and the query string) |
| Card fields, profile facts | 40 |
| Page links (buttons included) | 8, each a link path |
| Profile badges | 8 |
| Cards in a grid | 100; each card's profile as a profile, its link a link path |
| Actions | 4 in one `actions` value; each has a form id (not the id of a form on the page) and at most 10 hidden fields, names as for form fields |
| Progress | fraction 0 to 1; `from` and `to` both or neither, real instants, `to` after `from` |
| Code blocks | 16 KiB of text |
| Links to share | a link path, as for links |
| Skill levels | 0 to 5 trained; the level in training above that, at most 5 |
| Compositions | 1 to 8 parts, each amount above 0 and grade 0 to 4 |
| Defenses | shield, armor and hull each 0 to 1 |
| Timelines | a span of at most 60 days (`to` after `from`); 1 to 20 lanes, 50 events a lane, 60 windows; every time a real instant, an event's end after its start, links as link paths |
| Values (stats, cells, card fields) on a page | 10,000 |
| Any one piece of text | 2 KiB |
| The whole page: all text, link paths and times, plus 16 bytes per value | 1 MiB |
| ISK | a finite number |
| Times and countdowns | a real instant in RFC 3339, e.g. `2026-09-24T18:00:00Z`, at most 40 bytes |
| Link paths | at most 200 bytes of relative segments made of ASCII letters, digits, `-`, `_`, `.`: no leading or trailing `/`, no empty segments, no `.` or `..` segments, no scheme, query or fragment |

## Storage

A plugin that declares `storage = true` gets its own Postgres schema, and SQL through `tether_plugin_sdk::storage`. Create tables with migrations in the package (`migrations/0001_create_notes.sql`, `0002_...`). The host runs each migration once, in its own transaction, as your plugin's database role; a migration can't be changed once applied, so fix mistakes in a new one.

```rust
use tether_plugin_sdk::storage::{self, Statement, Value};

storage::execute("INSERT INTO notes (body, at) VALUES ($1, $2)", &["o7".into(), Value::timestamp("2026-09-24T18:00:00Z")])?;
let rows = storage::query("SELECT id, body FROM notes ORDER BY id DESC LIMIT $1", &[20.into()])?;
for row in &rows.rows {
    let body = row[1].as_text().unwrap_or("");
}
storage::transaction(&[
    Statement::new("UPDATE stock SET qty = qty - $1 WHERE item = $2", vec![5.into(), "Veldspar".into()]),
    Statement::new("INSERT INTO moves (item, qty) VALUES ($1, $2)", vec!["Veldspar".into(), (-5).into()]),
])?;
```

- Always pass data as `$1`, `$2`... parameters, never by formatting it into SQL. One call runs one statement (`transaction` runs several, all or none).
- Your schema is on the search path, so use plain table names. You can't see Tether's tables, other plugins' schemas or `public`, can't make temporary tables or other schemas, and can't use advisory locks.
- Values: `Null`, `Boolean`, `Integer` (int2/4/8), `Float` (float4/8), `Text` (text, varchar, char; dates read as `YYYY-MM-DD`), `Bytes` (bytea), `Timestamp` (RFC 3339; timestamptz, and timestamp read as UTC) and `Json` (json, jsonb). Cast anything else in SQL: `amount::text` for numeric, `id::text` for uuid.
- `rows.columns` names the columns; it's empty when there are no rows.
- Errors: `NotApproved` (no `storage = true`), `Invalid` (a limit or a type), `Database` (Postgres's SQLSTATE code, e.g. `23505` for a duplicate, and message), `Timeout`, `TooLarge`.

| Limit | Value |
| --- | --- |
| One statement | 5 s (`statement_timeout`); waiting for a lock, 2 s |
| SQL per statement | 64 KiB |
| Parameters | 100 per statement, 1 MiB per call in all |
| Statements in a `transaction` | 50 |
| A result | 5,000 rows and 4 MiB (text, bytes and JSON, plus 16 bytes per value) |
| Memory per sort or hash | 16 MB (`work_mem`); temporary files 256 MB |
| One migration | 60 s per statement, 2 minutes in all |

Setting these yourself (`SET`, `set_config`, `ALTER ROLE`) doesn't lift them: the host puts them back before every statement. Uninstalling a plugin deletes its schema and everything in it.

## Jobs

Work that shouldn't wait for a page view, such as syncing from ESI or a ping at a set time, runs as a job. There are two kinds, and both arrive at `run_job`:

- **Schedules**, declared in `plugin.toml` (`[[capabilities.schedules]]`, `every = "30m"`, from 5 minutes to 7 days). They run while the plugin is enabled. Tether also runs all of a plugin's schedules at once (restarting their intervals) when there's new data for it: when a character becomes one of the plugin's characters (registered for it, on an account holding one of its permissions), and when someone adds one of your data sources. Admins can run one by hand too. A schedule already queued isn't queued again (one running gets one more run queued behind it), nor one queued in the last minute; while ESI's budget is low, registrations and data sources anyone but an app admin added wait ten minutes instead, so a character that registers then may wait for the next tick. A sync job should read every character each time (`esi::characters()`), not only the ones it expects to be new. What ESI has cached is answered from the cache, so a run soon after another costs ESI little.
- **One-off jobs**, queued from a form submission or another job with `jobs::enqueue`. Give one a key to be able to move or cancel it: queuing under the same key replaces the queued job, and `jobs::cancel(key)` removes it.

```rust
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::{Page, PageError, Plugin, Request};

struct Moons;

impl Plugin for Moons {
    fn render(request: Request) -> Result<Page, PageError> { /* ... */ }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            "sync" => {
                // For each extraction: a ping when the chunk arrives. Queuing
                // again after a reschedule moves it.
                jobs::enqueue(
                    NewJob::new("ping")
                        .key("moon:40161234")
                        .payload(r#"{"moon": 40161234}"#)
                        .at("2026-09-30T18:05:00Z"),
                ).map_err(|e| JobError::Retry(format!("{e:?}")))?;
                Ok(())
            }
            "ping" => Ok(()),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}
```

- `job.scheduled_at` is when it was meant to run. After downtime, a restart or retries it runs late, so check it when timing matters (a ping for a chunk that arrived hours ago may not be worth sending).
- Return `JobError::Retry` for trouble that may pass (retried with backoff, up to 5 tries) and `JobError::Permanent` for a job that will never work. Hitting a limit or trapping counts as a retry.
- A job whose plugin is disabled or still loading waits for it without using up its tries; uninstalling removes all of the plugin's jobs.
- A time in the past means "as soon as possible": it runs after jobs already waiting, and `scheduled_at` still says the time you gave. One job of a plugin runs at a time, on workers apart from Tether's own and from page views.

| Limit | Value |
| --- | --- |
| One job run | 60 s in all, 10 s of CPU, 64 MiB of memory |
| Queued and running jobs | 1,000 per plugin (replacing a queued one by key still works at the limit) |
| Jobs created | 5,000 per plugin per day, finished ones included; finished jobs are kept a day |
| How far ahead | 120 days |
| Payload | JSON, at most 64 KiB |
| Name | lowercase letters, digits and `_`, at most 40 |
| Key | ASCII letters, digits and `. _ : -`, at most 100 |
| `enqueue` and `cancel` calls | 100 per form submission or job run (pages can't queue) |

## Forms

A form is part of a page. When someone posts it, the host checks the values against the form as your page draws it right then, and only then calls your `submit`:

```rust
use tether_plugin_sdk::{Field, Form, Page, PageError, Plugin, Request, SubmitResult, Submission};

fn render(request: Request) -> Result<Page, PageError> {
    Ok(Page::new("Alerts").form(
        Form::new("threshold", "Save")
            .field(Field::number("isk", "Ping above (ISK)").range(Some(0.0), None, true).required())
            .field(Field::select("ore", "Ore", vec![("ubiquitous".into(), "Ubiquitous".into()), ("r64".into(), "R64".into())]))
            .field(Field::checkbox("enabled", "Pings on", true)),
    ))
}

fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
    // Checked and written one way by the host: a whole number here.
    let isk: i64 = submission
        .value("isk")
        .parse()
        .map_err(|_| PageError::Failed("isk wasn't a number".into()))?;
    let on = submission.checked("enabled");
    // ... store it ...
    Ok(SubmitResult::Redirect("".into())) // or SubmitResult::Page(...) to show something
}
```

- Pages are read-only: `render` runs on plain page views, which a link on another site can trigger, so storage refuses writes there and `jobs::enqueue`/`cancel` fail. Change things in `submit` (checked for coming from Tether's own pages) or in jobs.
- Build a form's limits and options from what you've stored, never from the request: the host checks a post against the form your page draws for that same request.
- The host refuses, before `submit` is called: unknown fields, a field twice, missing required fields, text over its `max_length`, numbers that aren't numbers, out of range or (for `integer`) not whole, and select values that aren't one of the options. You get one value per field in the form's order: checkboxes as `true`/`false` (a required one must be ticked), empty optional fields as `""`, numbers written plainly (`100`, not `1e2`; whole numbers for `integer`).
- Posting needs the page's permission, comes from Tether's own pages only, and is limited to 30 a minute per person per plugin. No file uploads.
- Field names and form ids are lowercase letters, digits and `_`, starting with a letter. Up to 30 fields per form, 100 options per select, 10,000 characters per text field and 200,000 per textarea (a pasted inventory); a post is at most 512 KiB.
- Return `SubmitResult::Redirect(path)` to go to another of your pages (a link path), or `SubmitResult::Page(page)` to show a page there and then, for example the form again with a note about what to fix. A redirect may carry a query of its own (`fits?q=rifter`): `name=value` pairs, names lowercase letters, digits and `_`, values percent-encoded, at most 200 bytes and 10 pairs. Send a search there, so it stays in the address (and the page's row actions still match when one is posted: Tether checks an action against your render of the page with its query).

### Settings pages

A page that sets how your app behaves is one form in titled groups, saved at once from a bar Tether keeps at the bottom of the page (DESIGN.md, Save bar): each changed setting says CHANGED, the bar counts the changes and names their groups, Discard puts them back, and leaving with changes unsaved asks first. A save the host refuses leaves the page as it was, changes and all, with a toast saying why.

```rust
use tether_plugin_sdk::{Field, Page, SettingsForm, SettingsGroup};

Page::new("Settings").settings(
    SettingsForm::new("settings")
        .group(
            SettingsGroup::new("Discord")
                .description("Where pings go.")
                .field(Field::select("channel", "Post to", channels)),
        )
        .group(
            SettingsGroup::new("Fuel alerts")
                .field(Field::number("hours", "Warn under (hours)").range(Some(1.0), Some(168.0), true).required()),
        ),
)
```

- It posts as one form, named by the `SettingsForm`'s id, with every group's fields: `submit` gets them as it would a form's, checked the same way, one value per field in order. A field the person left as the page showed it comes with its value as your page draws it now, so a save never undoes another admin's change made meanwhile: store every value you get.
- 1 to 16 groups, each with a title and 1 to 30 fields, at most 120 fields in all, names unique across the groups. With more than one group Tether draws an index of them above.
- Keep what isn't a setting (adding a row, a one-off action) in forms and actions of its own on the page.

### Row actions

An action is a one-button form you put in a value, usually a table cell: Approve and Reject on a request, Close on a timer. Posting it calls your `submit` with `submission.form` set to the action's form and `submission.values` its hidden fields:

```rust
use tether_plugin_sdk::{Tone, action, actions};

// In a row:
actions(vec![
    action("Approve", "decide").field("request", "42").field("verdict", "approve").tone(Tone::Accent),
    action("Reject", "decide").field("request", "42").field("verdict", "reject")
        .tone(Tone::Danger)
        .confirm("The pilot is told their request was rejected."),
])

// In submit:
match submission.form.as_str() {
    "decide" => { let id = submission.value("request"); /* ... */ }
    _ => {}
}
```

- Before calling `submit`, Tether draws the page again for that person and checks it still has this very button: the same form and exactly the same fields and values. Anything else (another id, a value missing or added) is refused. So people can only post what you showed them, and hiding a button from someone is enough to keep them from using it. Still check permissions in `submit`, as for any form.
- Posting needs the page's permission, comes from Tether's own pages only, and counts toward the 30 posts a minute, like forms.
- `.tone(Tone::Danger)` for destructive actions, `Tone::Accent` for a region's one main action; others are outline buttons. `.confirm(sentence)` asks first, stating what will happen ("Its 4 members lose access"), never "Are you sure?".
- An action whose form id is one of the page's forms opens that form in a popup instead (the form isn't drawn on the page): its `.confirm(...)` sentence leads the popup, and the post carries the action's hidden fields and the form's values together (`submission.values`: the hidden ones first), checked against both, so the action's field names can't be the form's. Use it for a row action that needs a little input, such as how many runs. Hidden values can't hold line breaks or other control characters (browsers rewrite them, so they'd never post back as drawn).
- The check is against the page as `render` draws it for that request, so an action works only if your normal render of that page (path and query) shows it: not one that appears only on a page `submit` returned.
- Audited pages (`audit = true`) never reload themselves, whatever `refresh` says: each reload would be an audit entry.

## Who's looking

`identity::viewer()` says who is looking at a page or posting a form: their account id, main, all their characters (with corporation and alliance), access state (`viewer.state.name`, and `viewer.is_member()` / `viewer.is_guest()`; admins can add states above Member, such as a leadership state, so `is_member()` is false for them: gate on your own permissions rather than on state where you can), and which of your plugin's permissions they hold (`viewer.can("manage")`). Jobs have no viewer.

`identity::superuser()` says whether the viewer is a superuser (AA's `is_superuser`; false in jobs), for what AA shows superusers only, such as hrapplications' every corporation. Superusers also hold every permission, so `viewer.can(...)` is already true for them: use `superuser()` only where AA checks `is_superuser` itself, never as a stand-in for a permission admins should grant.

Groups, to limit something to them (allianceauth-fittings' categories are), with `groups = true` in `[capabilities]` (the admin sees it at install: the app learns which groups its users are in): `identity::groups()` is the viewer's own groups (`id` and `name`), Hidden and Internal ones included, and `identity::all_groups()` the groups to offer the viewer when they pick some, as Tether shows groups: every group that is neither Hidden nor Internal, plus the viewer's own; every group but Internal ones for holders of `group_management`; every group for holders of `admin.groups`. Both are empty in jobs, like the viewer, and without the capability. Store group ids, not names (groups can be renamed), and check membership against `groups()` on every view; a group someone else picked may not be in the viewer's `all_groups()`, so keep ids you don't recognise rather than dropping them.

Apps see an account only while its owner is looking: nothing else tells you which characters share an account (`esi::characters()` has no owners). `identity::owners()` and `identity::members()` are first-party only: they answer for Tether's bundled Member Audit (each member character's main and state, for aa-memberaudit's scopes by the owner's main; and every account holding one of its permissions with all its characters, registered or not, for its Character Finder, compliance reports and sheet) and return `None` for every other app, whatever its manifest says. There is no capability to ask for it; don't build on it.

## ESI

Plugins never see a token or build an ESI URL. You name an endpoint and whose token to use; the host checks, on every call, that:

- the endpoint is one Tether offers plugins (below) and its scope is declared in your `plugin.toml` and was approved;
- for a **user** scope (`capabilities.esi.user`): the character is one of your app's characters, as Alliance Auth's apps work: it is registered for your app, its account holds one of your permissions (any of them, in any state) and its token works. A token with the scopes granted for something else isn't a registration. Each call needs its own endpoint's scope in the token: add a user scope in a new version and the characters already registered stay yours, still read with the scopes they have, and a call needing the new one answers `MissingScope(scope)` until the pilot registers again (Tether asks them to). Skip that part of the character and carry on. Pilots register (and unregister) characters for your app themselves, through Tether's Register Character for it (`/register?app=<your id>`; a card grid's `.register()` card leads there), in one EVE login; installing your app requires nothing of anyone, and an admin may require your scopes of a state (the States page). Only scopes a character endpoint below uses are accepted. Keep the list short; each scope asks every pilot for more;
- for a **data-source** scope (`capabilities.esi.data_source`): the character is one of your app's data sources (AA's Add Owner): a holder of one of your `add_…` permissions (as AA's `add_refinery_owner`, `add_structure_owner`, `add_fatlink`; not `manage`), or of those you name instead in `capabilities.esi.owner_permissions` (for AA names that don't start `add_`, as aa-contacts' `manage_alliance_contacts`; 1 to 10 of your permissions, shown to the admin at install), or an app admin, added their own character, in one EVE login, and it's in use at once (no admin approval, as in Alliance Auth). It stays in use while the character is on that account and in the corporation it was added for. Corporation endpoints read that character's corporation. The host draws Add data source on your Data sources page (under Manage), and a notice on your other pages when a source stops working; `add_owner(label)` puts the same button in your own content, and brings the pilot back with `owner=<character id>` in the query (untrusted: check it). Declare an `add_…` permission if pilots should add owners; without one only app admins can, and the install review says which permissions add owners.

```rust
use tether_plugin_sdk::esi::{self, Subject};

// Corporation data, through each data source (e.g. a Station Manager's character).
for source in esi::data_sources() {
    for body in esi::get_all("corporation-mining-extractions", Subject::DataSource(source.id), &[])? {
        let extractions: Vec<serde_json::Value> = serde_json::from_str(&body)?;
    }
}
// Members' own data: every Member character registered with your scopes.
for character in esi::characters() {
    let skills = esi::get("character-skills", Subject::Character(character.id), &[], None)?;
}
let names = esi::names(&[40161234, 30000142])?;
```

| Endpoint | Scope | Subject | Paged | Params |
| --- | --- | --- | --- | --- |
| `corporation-mining-extractions` | `esi-industry.read_corporation_mining.v1` | data source | yes | |
| `corporation-mining-observers` | `esi-industry.read_corporation_mining.v1` | data source | yes | |
| `corporation-mining-observer` | `esi-industry.read_corporation_mining.v1` | data source | yes | `observer_id` |
| `corporation-structures` | `esi-corporations.read_structures.v1` | data source | yes | |
| `corporation-roles` | `esi-corporations.read_corporation_membership.v1` | data source | no | |
| `corporation-structure-notifications` | `esi-characters.read_notifications.v1` | data source | no | |
| `universe-system` | `esi-corporations.read_structures.v1` | data source | no | `system_id` |
| `corporation-starbases` | `esi-corporations.read_starbases.v1` | data source | yes | |
| `corporation-starbase` | `esi-corporations.read_starbases.v1` | data source | no | `starbase_id`, `system_id` |
| `corporation-customs-offices` | `esi-planets.read_customs_offices.v1` | data source | yes | |
| `corporation-structure-assets` | `esi-assets.read_corporation_assets.v1` | data source | no | |
| `corporation-asset-names` | `esi-assets.read_corporation_assets.v1` | data source | no | `item_ids` |
| `corporation-asset-locations` | `esi-assets.read_corporation_assets.v1` | data source | no | `item_ids` |
| `corporation-asset-places` | `esi-assets.read_corporation_assets.v1` | data source | no | `item_ids` |
| `corporation-hangar-assets` | `esi-assets.read_corporation_assets.v1` | data source | no | `structure_ids` |
| `corporation-wallets` | `esi-wallet.read_corporation_wallets.v1` | data source | no | |
| `corporation-divisions` | `esi-corporations.read_divisions.v1` | data source | no | |
| `source-contracts` | `esi-contracts.read_character_contracts.v1` | data source | yes | |
| `source-contract-items` | `esi-contracts.read_character_contracts.v1` | data source | no | `contract_id` |
| `corporation-blueprints` | `esi-corporations.read_blueprints.v1` | data source | yes | |
| `corporation-industry-jobs` | `esi-industry.read_corporation_jobs.v1` | data source | yes | |
| `fleet-members` | `esi-fleets.read_fleet.v1` | data source | no | |
| `killmail` | none (public) | any | no | `killmail_id`, `killmail_hash` |
| `universe-moon` | none (public) | any | no | `moon_id` |
| `universe-planet` | none (public) | any | no | `planet_id` |
| `sovereignty-systems` | none (public) | any | no | |
| `sovereignty-campaigns` | none (public) | any | no | |
| `corporation-contracts` | `esi-contracts.read_corporation_contracts.v1` | data source | yes | |
| `corporation-contract-items` | `esi-contracts.read_corporation_contracts.v1` | data source | no | `contract_id` |
| `source-structure` | `esi-universe.read_structures.v1` | data source | no | `structure_id` (a structure's name, system and type, as the data source sees it) |
| `corporation-contacts` | `esi-corporations.read_contacts.v1` | data source | yes | |
| `corporation-contact-labels` | `esi-corporations.read_contacts.v1` | data source | no | |
| `alliance-contacts` | `esi-alliances.read_contacts.v1` | data source | yes | |
| `alliance-contact-labels` | `esi-alliances.read_contacts.v1` | data source | no | |
| `universe-constellation` | none (public) | any | no | `constellation_id` |
| `esi-status` | none (public) | any | no | |
| `character-affiliation` | none (public) | any | no | `character_ids` (1 to 1,000, comma-separated) |
| `character-skills` | `esi-skills.read_skills.v1` | character | no | |
| `character-skillqueue` | `esi-skills.read_skillqueue.v1` | character | no | |
| `character-ship` | `esi-location.read_ship_type.v1` | character | no | |
| `character-assets` | `esi-assets.read_assets.v1` | character | yes | |
| `character-wallet` | `esi-wallet.read_character_wallet.v1` | character | no | |
| `character-wallet-journal` | `esi-wallet.read_character_wallet.v1` | character | yes | |
| `character-clones` | `esi-clones.read_clones.v1` | character | no | |
| `character-implants` | `esi-clones.read_implants.v1` | character | no | |
| `character-location` | `esi-location.read_location.v1` | character | no | |
| `character-online` | `esi-location.read_online.v1` | character | no | |
| `character-wallet-transactions` | `esi-wallet.read_character_wallet.v1` | character | no | optional `from_id` |
| `character-contracts` | `esi-contracts.read_character_contracts.v1` | character | yes | |
| `character-contract-items` | `esi-contracts.read_character_contracts.v1` | character | no | `contract_id` |
| `character-contacts` | `esi-characters.read_contacts.v1` | character | yes | |
| `character-standings` | `esi-characters.read_standings.v1` | character | no | |
| `character-mail` | `esi-mail.read_mail.v1` | character | no | optional `last_mail_id` |
| `character-mail-body` | `esi-mail.read_mail.v1` | character | no | `mail_id` |
| `character-mail-labels` | `esi-mail.read_mail.v1` | character | no | |
| `character-mailing-lists` | `esi-mail.read_mail.v1` | character | no | |
| `character-loyalty-points` | `esi-characters.read_loyalty.v1` | character | no | |
| `character-planets` | `esi-planets.manage_planets.v1` | character | no | |
| `character-planet` | `esi-planets.manage_planets.v1` | character | no | `planet_id` |
| `character-industry-jobs` | `esi-industry.read_character_jobs.v1` | character | no | |
| `character-blueprints` | `esi-characters.read_blueprints.v1` | character | yes | |
| `character-orders` | `esi-markets.read_character_orders.v1` | character | no | |
| `character-killmails` | `esi-killmails.read_killmails.v1` | character | yes | |
| `killmail-detail` | none (public) | any | no | `killmail_id`, `killmail_hash` |
| `character-corporation-history` | none (public) | any | no | `character_id` |
| `character-attributes` | `esi-skills.read_skills.v1` | character | no | |
| `character-fatigue` | `esi-characters.read_fatigue.v1` | character | no | |
| `character-roles` | `esi-characters.read_corporation_roles.v1` | character | no | |
| `character-titles` | `esi-characters.read_titles.v1` | character | no | |
| `character-notifications` | `esi-characters.read_notifications.v1` | character | no | |
| `character-calendar` | `esi-calendar.read_calendar_events.v1` | character | no | optional `from_event` |
| `character-calendar-event` | `esi-calendar.read_calendar_events.v1` | character | no | `event_id` |
| `character-fittings` | `esi-fittings.read_fittings.v1` | character | no | |
| `character-mining` | `esi-industry.read_character_mining.v1` | character | yes | |
| `universe-structure` | `esi-universe.read_structures.v1` | character | no | `structure_id` |
| `universe-station` | none (public) | any | no | `station_id` |
| `character-public` | none (public) | any | no | `character_id` |
| `universe-category` | none (public) | any | no | `category_id` |
| `universe-group` | none (public) | any | no | `group_id` |
| `markets-prices` | none (public) | any | no | |
| `universe-ids` | none (public) | any | no | `names` |
| `universe-type` | none (public) | any | no | `type_id` |

- The body is ESI's JSON, at most 4 MiB; `pages` says how many pages a paged endpoint has. At most 100 ESI calls per submit or job run, 20 per page render.
- Errors: `NotAllowed` (endpoint or scope), `NotRegistered` (not one of your app's characters: not registered for it, or its account holds none of your permissions), `MissingScope(scope)` (one of your characters, but its token lacks the scope this endpoint needs, which you asked for after it registered: skip what needs it; from `post`, and from `get` in a package built with an SDK older than `fetch`, this is `NotRegistered` though `characters()` lists the character, so don't drop a character's data on `NotRegistered` alone), `NotADataSource`, `Token` (the character must log in again), `Status(code)` from ESI, `Invalid`, `TooLarge`, `Unavailable`. Plan for `NotRegistered`, `MissingScope` and `Token`: people leave, and revoke tokens. To show or log one, use `esi::describe(&err)` ("ESI refused (403): the character lacks an in-game role or a scope"), never `{err:?}`: pages say what went wrong in words.
- Corporation endpoints also need the character to hold the in-game role CCP requires (Station Manager for extractions and structures, Accountant for observers, Director or Personnel Manager for roles, Director for starbases, customs offices and assets); without it ESI answers 403. `universe-system` is public data, read through a data source only because `names` doesn't say which region a system is in; it answers the system's `name`, `security_status`, `constellation_id`, `region_id` and `planets` (ids); it makes two ESI requests (system, then constellation) and costs 2 of the per-run limit.
- `universe-moon` (`moon_id`, `name`, `system_id`, `position`), `universe-planet` (`planet_id`, `name`, `system_id`, `type_id`, `position`) `universe-constellation` (`constellation_id`, `name`, `region_id`), `sovereignty-systems` (`[{"system_id", "alliance_id", "adm"}]`, systems alliances hold, with their activity defense multiplier) `esi-status` (`{"compatibility_date", "routes": [{"method", "path", "status"}]}`, ESI's own status route by route: OK, Degraded, Down, Recovering or Unknown) and `sovereignty-campaigns` (`[{"campaign_id", "event_type", "solar_system_id", "constellation_id", "structure_id", "defender_id", "defender_score", "attackers_score", "start_time"}]`, ESI's as they are, read loosely so a new event type doesn't break it) are public, like `killmail`: `names` doesn't do moons, planets or a constellation's region.
- `corporation-starbase` answers only the starbase's `fuels` (`[{"type_id", "quantity"}]`), not its roles or defence settings. `corporation-starbases` and `corporation-customs-offices` are ESI's lists as they are.
- `corporation-hangar-assets` answers what the corporation keeps in its hangars at the given `structure_ids` (1 to 100 stations or structures), from the same background read as `corporation-asset-places` (unavailable until ready; a call costs 2): `{"stock": [{"structure_id", "location_flag" (Hangar, CorpSAG1-7), "container_id" (null: loose in the hangar), "type_id", "quantity"}], "containers": [{"item_id", "type_id", "structure_id", "location_flag", "holds_items"}]}`, an office's hangars placed at its structure. Names of containers come from `corporation-asset-names`. `corporation-wallets` (Accountant) and `corporation-divisions` (Director) are ESI's as they are. `source-contracts` and `source-contract-items` are the data source character's own contracts, not its corporation's.
- Tether's built-in copy of CCP's static data answers the `sde-*` endpoints. Any app may call them, with `Subject::Character(0)`, no token and no scope; they make no ESI call but count as one of the run's calls. They answer as JSON:
  - `sde-types`: `ids=` (comma-separated, up to 1,000), or one `name=` per exact name as the game's inventory writes it (up to 1,000; a paste's names). Each type found: `type_id`, `name`, `published`, `group_id`, `group_name`, `category_id`, `category_name`, `market_group_id`, `market_group_chain` (its market group and every ancestor, up to the root), `volume`, `packaged_volume`, `portion_size`, `meta_level` and `compressed_type_id` (an ore's or ice's compressed form).
  - `sde-type-search`: `q=`, published types by name, names starting with it first; `limit=` up to 50; `exclude_category=` ids to leave out (9: blueprints).
  - `sde-materials`: `ids=`, what one portion (`portion_size` units) of each reprocesses into, as `[{"type_id", "portion_size", "materials": [{"type_id", "quantity"}]}]`.
  - `sde-groups` and `sde-market-groups`: `ids=` or `q=`. Market groups come with their `chain` of ancestors and a `path` ("grandparent -> parent -> name").
  - `sde-systems`: `ids=` or `q=`, solar systems' names.
- `corporation-structure-assets` is the corporation's assets trimmed to what sits in structures' slots and bays (`ServiceSlot0`-`7`, `StructureFuel`, `QuantumCoreRoom`, `MoonMaterialBay`; and `HiSlot`, `MedSlot`, `LoSlot`, `RigSlot`, `FighterTube` and `FighterBay`, which ships share, only for items in the corporation's own Upwell structures, checked against `corporation-structures`, so the character needs that scope and the Station Manager role for those) and Orbital Skyhooks in space, each as `item_id`, `type_id`, `location_id`, `location_flag`, `location_type`, `quantity`; never hangars, cargo, deliveries or ships. It answers from every page of the corporation's assets, however many (up to 2,000), read in the background as for `corporation-asset-places` (the same read, kept an hour for that data source; ESI checks the character's roles on every call): until a read is ready the call answers `Unavailable`, so ask again on a later run and keep what you had. It isn't paged, and a call costs 2. `corporation-asset-names` and `-locations` take up to 1,000 comma-separated `item_ids` of the corporation's own items and answer `[{"item_id", "name"}]` and `[{"item_id", "position": {"x", "y", "z"}}]`.
- `corporation-blueprints` (ESI's list as it is: `item_id`, `type_id`, `location_id`, `location_flag`, `material_efficiency`, `time_efficiency`, `quantity`, and `runs`, -1 for an original) and `corporation-industry-jobs` (running jobs only, ESI's as they are) need the Director and Factory Manager roles. A blueprint's `location_id` is often a container or a corporation hangar's office, not a place: `corporation-asset-places` takes up to 1,000 of the corporation's own `item_ids` and answers, for each found, in id order, `{"item_id", "type_id", "location_flag", "within": [{"type_id", "location_flag"}], "place_id", "place_type"}`: the containers and hangars holding it, innermost first, then the station, structure or system at the top (`place_type` `station`, `solar_system`, or `item` for an Upwell structure, the corporation's own included: an office or deliveries hangar is placed in its structure, not the structure's system). Nothing about items you didn't ask about. Tether reads every page of the corporation's assets in the background, outside your call's time, with the data source's login, and keeps that read an hour for that data-source character: calls for it then answer from it, for 2 of the 100 calls. Each such call still asks ESI for the first page, so a character that lost the Director role is refused (a 403) at once. With no read from the last hour, the call starts one and answers `Unavailable`: ask again in a minute (queue a job), as a large corporation's assets take minutes to read. A read that failed answers with its error (a 403 without the Director role) for five minutes. A corporation with more than 2,000 pages of assets isn't read: `Invalid`, saying so. Ask for all your ids in one call, not one at a time; aa-blueprints reads places twice a day.
- `corporation-contacts` and `alliance-contacts` (with their `-labels`) are the data source's corporation's and alliance's contacts (`contact_id`, `contact_type`, `standing`, `label_ids`; types read as text): the alliance is the data-source character's own, found by the host; with none, `alliance-*` answers `Invalid`.
- `corporation-structure-notifications` is the data-source character's own notifications, only the types aa-structures relays: its corporation's structures and moon drills (`StructureUnderAttack`, `StructureLostShields`, `StructureLostArmor`, `StructureDestroyed`, `StructureFuelAlert`, `StructureServicesOffline`, `StructureWentLowPower`, `StructureWentHighPower`, `StructureOnline`, `StructureAnchoring`, `StructureUnanchoring`, `OwnershipTransferred`, `StructuresReinforcementChanged`, the Metenox's `StructureLowReagentsAlert` and `StructureNoReagentsAlert`, starbases' `TowerAlertMsg` and `TowerResourceAlertMsg`, customs offices' `OrbitalAttacked` and `OrbitalReinforced`, the `Skyhook...` ones and the `Moonmining...` extraction ones), sovereignty and its bills (`SovStructureReinforced`, `SovStructureDestroyed`, `EntosisCaptureStarted`, `SovCommandNodeEventStarted`, `SovAllClaimAquiredMsg`, `SovAllClaimLostMsg`, `AllAnchoringMsg`, `InfrastructureHubBillAboutToExpire`, `IHubDestroyedByBillFailure`, `BillOutOfMoneyMsg`, `CorpAllBillMsg`), wars (`WarDeclared`, `DeclareWar`, `WarInherited`, `WarAdopted` (ESI spells it with a trailing space; you get it trimmed), `AcceptedAlly`, the `AllyJoinedWar...` ones, `AllWarCorpJoinedAllianceMsg`, `AllWarSurrenderMsg`, `CorpWarSurrenderMsg`, `OfferedSurrender`, `WarSurrenderOfferMsg`, `OfferedToAlly`, `MercOfferedNegotiationMsg`, `MercOfferRetractedMsg`, `WarHQRemovedFromSpace`, `WarInvalid`, `WarRetractedByConcord`, `CorpBecameWarEligible`, `CorpNoLongerWarEligible`), and the corporation's members, applications and projects (`CorpAppNewMsg`, `CorpAppInvitedMsg`, `CharAppWithdrawMsg`, `CharAppRejectMsg`, `CorpAppRejectCustomMsg`, `CharAppAcceptMsg`, `CharLeftCorpMsg`, `CorporationGoalCreated`, `CorporationGoalCompleted`, `CorporationGoalClosed`). Each has only `notification_id`, `type`, `timestamp`, `text` (EVE's YAML) and `sender_id`. Types are read as text, so one CCP adds later doesn't break the read. ESI caches them for 10 minutes.
- `fleet-members` reads the fleet the data-source character is in, found by the host from that character's own token (a plugin never names a fleet): `{"in_fleet": false, "boss": false}` when it isn't in one, `{"in_fleet": true, "boss": false}` when it isn't the fleet boss, else `{"in_fleet": true, "boss": true, "fleet_id", "members": [{"character_id", "ship_type_id", "solar_system_id", "join_time"}]}`. FCs add their fleet boss as the app's data source from Create FAT Link (`add_owner`), as aa-afat asks only FCs for the scope. "Boss" is as of ESI's cached answer (a few seconds), and the members may come from Tether's shared cache, so a character that just passed boss can read them once more. It makes two ESI requests, so it counts as two of the 100 (or 20) calls. "Not in a fleet" is ESI's 404, and counts as an error toward your plugin's ESI error throttle; poll no more than once a minute.
- `killmail` is public: no scope, no token, any plugin, and the subject you pass isn't used (`Subject::Character(0)` is fine). Give the id and 40-hex-digit hash a killmail link carries (zKillboard's API gives the hash for an id). It answers `killmail_id`, `killmail_time`, `solar_system_id`, `victim` (`character_id`, `corporation_id`, `alliance_id`, `ship_type_id`; no `character_id` for structures) and `attackers` (how many).
- The character endpoints are a full character viewer (SeAT's, aa-memberaudit's). Each reads only the character you name, with its own token, and answers ESI's JSON as it is, except `universe-structure`. Mail comes in two steps: `character-mail` gives headers (subject, sender, recipients, labels, read; the newest 50, then the 50 before `last_mail_id`), and `character-mail-body` gives one mail's body by the `mail_id` a header gave, one mail per call. `character-wallet-transactions` steps back with `from_id` and `character-calendar` forward with `from_event`, as ESI does; they have no pages. `character-industry-jobs` includes jobs finished in the last 90 days. `character-killmails` gives `killmail_id` and `killmail_hash` pairs; read each with the public `killmail-detail` (the whole killmail: victim, its items, every attacker) or `killmail` (its short form). `character-notifications` is every notification the character has (the last 500 or 30 days), unlike `corporation-structure-notifications`. `universe-structure` answers `structure_id`, `name`, `solar_system_id` and `type_id` of an Upwell structure the character may dock at: use it to show where a clone, asset or ship is. ESI names a structure only to a character that may dock there; when it refuses yours (here or through `source-structure`), Tether asks through other members' characters that granted the structure scope and answers with what they could read, or the 403 if none could (a structure's name is kept for every app a week). `universe-station` (an NPC station), `character-corporation-history` (anyone's corporations, by `character_id`) and `character-public` (anyone's public sheet: `birthday`, `security_status`, `bloodline_id`, `race_id`, `faction_id`, `gender` and `description`, the bio) are public, as are `universe-category` (`category_id`, `name`, `groups`; 16 is skills) and `universe-group` (`group_id`, `name`, `category_id`, `types`), which say which group a skill is in.
- `markets-prices` is public too: CCP's price of every type, `[{"type_id", "average_price", "adjusted_price"}]` (either price may be missing), one list of about 15,000 entries that ESI caches for an hour. Read it once a day and keep the prices you need in storage.
- `universe-ids` turns exact names into ids (ESI's `POST /universe/ids`): give `names` one per line, at most 500, each at most 100 characters (blank lines are skipped, repeats sent once). It answers ESI's object as it is: `inventory_types` (items and ships, `[{"id", "name"}]`, the name as EVE writes it), `characters`, `corporations`, `systems` and the rest, each only when something matched; a name nothing matched is just missing. `universe-type` answers an item type by `type_id` as ESI does: `name`, `group_id` (`universe-group` says its category), `published`, `dogma_attributes` (`[{"attribute_id", "value"}]`; 182 to 184 are the required skills' type ids and 277 to 279 their levels) and the rest. Both are one call each; types don't change between patches, so keep what you read in storage.
- Values CCP adds to ESI's lists after this Tether was built (a new location flag, role, notification or contract type) don't break a read of the endpoints from `character-wallet-transactions` on, nor of `character-assets`, `character-wallet-journal` or `corporation-structure-assets`: the answer is read a second time as it is, and they pass through as text. That second read counts as one more of the 100 (or 20) calls, and is skipped (`Unavailable`) while ESI's error budget is low. Ids you give are positive numbers; anything else is `Invalid`, without asking ESI.
- Every call is recorded in your plugin's access log, which admins see. An admin must also enable your scopes on Tether's EVE application.

### Writes

A few endpoints change something in EVE. Call them with `esi::post(endpoint, Subject::Character(id), body)`; `esi::get` can't reach them, and they aren't in the table above.

| Endpoint | Scope | Body |
|---|---|---|
| `character-fitting-save` | `esi-fittings.write_fittings.v1` | ESI's fitting JSON: `name` (1 to 50 characters), `description` (at most 500), `ship_type_id`, and `items`, 1 to 512 of `{"flag", "quantity", "type_id"}` with ESI's slot flags (`HiSlot0`, `MedSlot0`, `LoSlot0`, `RigSlot0`, `SubSystemSlot0`, `ServiceSlot0`... `DroneBay`, `FighterBay`, `Cargo`) and a quantity of 1 to 1,000,000. Answers `{"fitting_id"}`. |

- Only in `submit`, while a pilot posts one of your forms: never in `render` or a job (`NotAllowed`). The pilot presses your button; you save to their character.
- Only for one of that pilot's own characters (`identity::viewer()`'s `characters`) that is one of your app's characters (registered for it with the write scope): anyone else's is `NotAllowed`, an unregistered one `NotRegistered`.
- The host parses the body into ESI's type and sends that: a body that isn't ESI's fitting is `Invalid` before anything reaches ESI. At most 64 KiB.
- Sent once, never retried: a failure saves nothing, and the pilot may press again. Each write is in your access log and, as the pilot, on the audit log.
- A write scope can't be required of a state: an app asking for one can't be required on the States page.

## Discord

With `discord = ["send_message"]`, a plugin can post to the channels an admin assigned it (`discord::channels()`):

```rust
use tether_plugin_sdk::discord::{self, Mention};
let channel = discord::channels().first().map(|c| c.id.clone());
discord::send(&channel.unwrap(), "Moon popped at 1DQ1-A I", Mention::State("Member".into()))?;
```

Or a card (Discord's embed), as notification bots post them:

```rust
use tether_plugin_sdk::discord::{self, Embed, Image, Mention};
let card = Embed::new("Extraction started")
    .description("The chunk arrives <t:1793592000:R>.")
    .color(0x2ecc71)
    .author("Acme Corp", Some(Image::Corporation(98000001)))
    .thumbnail(Image::TypeRender(35835))
    .field("System", "Jita")
    .wide_field("Structure", "Jita - Refinery")
    .footer("Moon Mining")
    .timestamp("2026-11-02T04:00:00Z");
discord::send_embed(&channel, &card, Mention::State("Member".into()))?;
```

- Mentions are only the Discord role Tether maps to a state, named like `Mention::State("Member".into())`; typed `@everyone` and `@here` are defused, and nobody else can be pinged. A card's mention is the message's text above it.
- A card's title can open one of your app's own pages: `discord::send_linked_embed(&channel, &card, "fleet/12", Mention::None)` takes the page's path as `[[pages]]` give it (a query allowed), and Tether makes the address on this instance, so a card never links anywhere else (a path that isn't one, such as `/x`, `../x` or a full address, is `Invalid`).
- A card's images are CCP's, by id (`Image::Character`, `Corporation`, `Alliance`, `TypeRender`, `TypeIcon`): the host makes the link. Its limits are Discord's: a title, author, footer and field names of 256 characters, a description of 2,000, 10 fields with values of 1,024, 6,000 in all. `<t:UNIX:R>` in the text shows a time counting down in each reader's clock. Markdown links in the description and fields go where you point them, as in plain messages.
- From `submit` and jobs only, not pages. At most 1,500 characters, 5 messages per call and 20 a minute per plugin (cards count the same).
- Errors: `NotAllowed(why)` is final until an admin changes something: not one of your channels, no role mapped to that state, or Discord refused the bot there (no access to the channel, or it was deleted). Mark that message failed and go on to the next. For a minute after Discord refuses the bot in a channel, your sends there get the same answer without reaching Discord, and don't count against the 20. `Invalid(why)` breaks a limit. `RateLimited` and `Unavailable` (Discord down or slow) mean try later.

## HTTP

With `http = ["zkillboard.com"]` in `[capabilities]`, a plugin can call those exact hosts over HTTPS, once an admin approved them at install. Nothing else is reachable: not other hosts, not plain HTTP, not other ports, not IP addresses.

```rust
use tether_plugin_sdk::http;

let answer = http::get_json("https://zkillboard.com/api/killID/128570923/")?;
if answer.is_success() {
    let kills: Vec<serde_json::Value> = serde_json::from_slice(&answer.body)?;
}
// A POST with a secret the admin entered (declared as [capabilities.secrets.janice_api_key]):
let appraisal = http::Request::post("https://janice.e-351.com/api/rest/v2/appraisal?market=2", b"Tritanium 100".to_vec())
    .header("content-type", "text/plain")
    .secret("janice_api_key")
    .send()?;
let etag = appraisal.header("etag");
```

- `http::get`, `http::get_json` and `http::post_json` cover most calls; `http::Request::get(url)` / `::post(url, body)` with `.header(...)` and `.secret(name)`, then `.send()`, for the rest. A response is its `status`, a few `headers` and the `body` bytes (`text()` for UTF-8), whatever the status: a 404 or 500 is an answer, not an error.
- You may set only `accept`, `accept-language`, `content-type`, `if-none-match` and `if-modified-since` (at most 10, 256 printable ASCII characters each). Never `Authorization`, `Cookie`, `Proxy-*`, `Host` or `User-Agent`, nor any header one of your secrets goes in: Tether sets the User-Agent (`tether (app <your id>; +<the instance's public URL>)`, a way to reach the operator, as APIs such as zKillboard ask), and adds a secret you name with `.secret(...)`, in its declared header with its prefix, only on requests to its declared host. The value never reaches your plugin; ask for a secret on another host and the request is refused.
- Responses carry only `content-type`, `etag`, `last-modified`, `cache-control`, `expires`, `age` and `retry-after` (lowercase), never `Set-Cookie`.
- Tether follows redirects itself, only to your approved hosts (a secret only ever goes to its own host), at most 3 in a row. A redirect anywhere else fails with `NotAllowed`.
- Pages can only GET (a page view can be triggered by a link on another site); POST from `submit` or a job.
- Errors: `NotAllowed(why)` (host, scheme, header, method, secret, redirect), `TooMany` (a limit below), `TooLarge`, `Timeout`, `Unavailable`.
- Every request (method, host, path, status, size, time, and the secret's name if one was added) is in your plugin's HTTP log, which admins see. The query string isn't kept.
- Be a good citizen: cache answers in storage, use `if-none-match` with the `etag` you got, and respect `retry-after`. Many APIs ask for gentle rates.
- A new version that adds hosts or changes its secrets needs the admin's approval again; until then the new ones are refused.

| Limit | Value |
| --- | --- |
| Requests | 20 per submit or job run, 5 per page render; 60 a minute and 5,000 a day per plugin, refused attempts included |
| Request body | 64 KiB, POST only |
| Response body | 1 MiB (larger fails with `TooLarge`) |
| One request | 10 s (each redirect hop) |
| URL | 2,048 characters, `https://` on port 443 |

## Secure Groups filters

Offer filters admins can put on smart groups (aa-securegroups takes skills, assets and FATs from apps). Declare them in `plugin.toml`:

```toml
[[filters]]
name = "fats"
label = "FATs in the last days"   # shown to admins and, as a requirement, to pilots
combine = "sum"                   # "any": one character passing is enough (report 1 or 0);
                                  # "sum": characters add up and must reach an admin-chosen total

[[filters.fields]]                # at most 5; admins fill them in per smart group
name = "days"
label = "Days"
kind = "number"                   # or "text"
```

From a job (hourly is usual; values over two days old count as unknown, and a group with an unknown filter is left alone), ask which settings groups use and report a value per character from your own data:

```rust
use tether_plugin_sdk::filters;

for setting in filters::wanted() {        // setting.config: the admin's fields, a JSON object
    let values: Vec<(i64, i64)> = compute(&setting.name, &setting.config);
    filters::report(&setting.name, &setting.config, &values)?;
}
```

- Tether combines characters into accounts; this interface never tells you which characters share one.
- Report every character you have complete data for, 0s included: a reversed filter passes only when every character of an account was reported, so leave out characters you can't judge rather than guessing 0.
- Values are 0 to 1,000,000,000; at most 100,000 per report, 50 reports per call, one character once per report. Only settings in `wanted()` are accepted. Not from pages.

## Shared timers

Timers one app publishes and another shows (aa-structures feeding the timerboard). With `timers = "publish"` under `[capabilities]`, `timers::publish(&[Timer { key, title, at, system, details, objective, corporation_id }])` replaces your published timers (at most 500; `at` in RFC 3339 EVE time; objective `friendly`, `hostile` or `neutral`; `corporation_id` makes it corporation-only). With `timers = "read"`, `timers::published()` returns every running app's timers that ended at most a day ago, with the source app's name; corporation-only ones only for a viewer whose main is in that corporation, and none in jobs. Not from pages (publishing).

## Shared doctrines

Doctrines one app publishes for others to offer (allianceauth-fittings' doctrines in aa-fleetpings and aa-fat). With `doctrines = "publish"` under `[capabilities]`, `doctrines::publish(&[Doctrine { key, name, link, groups }], see_all)` replaces your published doctrines (at most 500, in the order given): `key` your own id (1 to 100 characters, each once), `name` (1 to 100, one line), `link` one of your pages (a link path), and `groups` `None` for everyone or `Some(ids)` (`identity::groups` ids, at most 100) for members of any of them. `see_all`, one of your own permissions or `None`, sees every one (Fittings passes `Some("manage")`, AA's `fittings.manage`). Not from pages. With `doctrines = "read"`, `doctrines::published()` returns the running apps' doctrines the viewer may see, each with its name, its page's address (`/plugins/<id>/<link>`) and the source app's name; none in a job. Tether's Fleet Pings offers them too, when an admin turns on "Use doctrines from Fittings".

## Downloads

CSV files your app offers for download (aa-memberaudit's data exports). With `downloads = true` under `[capabilities]`, build a file in a job or submit: `let build = downloads::begin(name, title, permission, &header)?`, then `downloads::append(name, build, &rows)` as often as needed (at most 5,000 rows a call, each with as many cells as the header, at most 10,000 calls), then `downloads::finish(name, build)`. The finished file replaces the last one only at `finish`, so pilots always get a whole file. Calling `begin` again starts a newer build and drops an unfinished one: the older build's `append` and `finish` then return `Error::Superseded`, and that chain should stop quietly (carry `build` in your continuation jobs' payloads, and give them their own job keys, so a new start and a continuation don't replace each other). `name` is 1 to 50 lowercase letters, digits and dashes; `title` one line of up to 100 characters; `permission` one of your own permissions, which a pilot needs to download it. At most 20 downloads, 50 MB each, 60 columns, 10,000 characters a cell.

Tether writes the CSV itself: cells are quoted as needed, lines end CRLF, and a cell a spreadsheet would run as a formula (starting `=`, `+`, `-`, `@`, a tab or a carriage return, and not a number) gets a leading `'`. It serves the file at `/plugins/<id>/downloads/<name>` (so `downloads/` isn't one of your page paths); link to it as `downloads/<name>` from your pages, and Tether renders the link as a download. Pilots without the permission, or without a main, don't get it; every download is audited. Downloads aren't in Tether's snapshots and backups, so after a rollback they're gone until your next build. `downloads::files()` lists your finished files (name, title, rows, when built) for your pages. Build large files over several jobs (enqueue the next chunk from each), since each call has its time limits. Not from pages.

## Notices

Notices in Tether's notifications (the bell), as AA apps `notify` (aa-blueprints tells requesters and approvers about requests this way). With `notify = true` under `[capabilities]`, from a submit or a job: `notify::account(account_id, title, message, level)` reaches one account, an `account_id` from `identity::current()` kept from when they used your app (a request's requester, say), and returns false when it no longer holds any of your permissions; `notify::holders(permission, title, message, level, except)` reaches every holder of one of your own permissions but `except` (the one who acted) and returns how many. To reach someone who submitted one of your forms but may hold none of your permissions (an applicant on a `signed_in` page, a requester who has since lost access), call `notify::submitter_reference()` in `submit` while they post it, keep the reference with what they submitted, and later `notify::submitter(reference, title, message, level)` (false if their account is gone, it's past its year, or the hourly limit for them is reached). The reference is the same every time for one account, means nothing to another app, reaches them for a year after the last post you asked for it in, and is refused in pages and jobs. Tether puts your app's name before the title (`Blueprints: ...`), so write titles without it; plain text only, the title at most 100 characters and the message at most 1,000. A notice already waiting unread isn't sent again. Limits: 10 notify calls a call, 20 notices an hour to one account from your app (past that it's skipped, and `account` returns false), 500 an hour from your app in all (past that you get `Error::Invalid`), and `holders` refuses a permission held by more than 200. Not from pages.

## Logging

`log::debug`, `log::info`, `log::warn` and `log::error` write to the plugin's log, which admins see on the plugin's page (the newest 1,000 lines are kept). The host keeps the first 100 lines per call, each cut to 1,024 characters, with control characters and invisible formatting characters replaced. The text of `PageError::Failed` is treated the same way. Never log anything personal you don't need.

## Limits

Every call runs in a fresh sandbox; nothing is kept between calls (state goes in storage). Per call:

| Limit | Default |
| --- | --- |
| Memory, all of it | 64 MiB |
| CPU time | 2 s |
| Whole call, including host calls | 10 s |
| Calls of one plugin at once | 2 (more wait) |
| Component size | 32 MiB |
| Data copied out of the plugin per call (its page, its log lines) | 16 MiB; past it, the call fails |

There is no filesystem, no network access of your own (only `http` through the host), no environment variables and no stdio (writes to stdout and stderr are discarded; use `log`). Clocks and random numbers work. A plugin that imports the filesystem or sockets (for example by using `std::fs` or `std::net`) is refused at load, as is one that defines its own component resource types.

Hitting a limit ends that call only; the next call starts clean. Admins see which limit was hit.

## What the host offers

API version 1 (`host_api = "1"` in `plugin.toml`, WIT package `tether:plugin@1.0.0`) is unstable until Tether's milestone 3 ends. After that, nothing in 1.x changes: new things arrive as new types, functions or interfaces, so a plugin built against an earlier 1.x keeps loading. Today it has:

- `log`: write to the plugin's log;
- pages: `render`, and forms: `submit`;
- `storage`: SQL in the plugin's own schema (see Storage);
- `jobs`: schedules and one-off jobs (see Jobs);
- `identity`, `esi` and `discord` (see above);
- `http`: HTTPS to hosts an admin approved (see HTTP);
- `filters`, `timers` and `doctrines`: Secure Groups filters, shared timers and shared doctrines (see above);
- `downloads`: CSV files to download (see Downloads);
- `notify`: notices in the bell (see Notices).

## Checklist before publishing

- Builds with `cargo build --target wasm32-wasip2 --release` and passes `cargo clippy --target wasm32-wasip2 -- -D warnings`.
- Every `render` path returns quickly; long work belongs in background jobs.
- Pages stay under the limits above for your biggest real data.
- No `std::fs`, `std::net` or `std::env`.
