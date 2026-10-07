# Admin guide

This guide is for whoever runs a Tether instance: the people who install it, set it up, and decide who gets in and what they may do. Pilots who only use Tether want the [member guide](members.md). App authors want the [app developer guide](app-developers.md).

Admins sign in like everyone else, with EVE Online. There is no separate admin login. What you may do comes from your permissions, and a superuser holds every permission.

Everything an admin changes is under **Administration** in the sidebar. Its overview lists the pages you may open, in four groups:

- **Access**: States, Groups, Auto Groups, Permissions, Permissions Audit.
- **Members**: Users, Blacklist, Compliance Report, Corporation Stats.
- **Integrations**: Discord, Fleet Pings, Apps, Data sources.
- **Instance**: Health, Settings, Menu, Audit log, Setup.

Some actions are sensitive: making a superuser, granting a sensitive permission, approving an app, upgrading Tether, changing the Discord settings, and a few more. Tether asks for these to be confirmed with a fresh EVE login. You see a **Confirm it's you** page, log in with your main, and come back to the page you were on. Submit the action again there; nothing is sent for you. A login counts for 15 minutes.

## Install

[deploy/README.md](../../deploy/README.md) covers installing, choosing a reverse proxy, upgrading and rolling back. In short: you need a server with Docker and a domain pointing at it. One command, `deploy/install.sh alliance.example.com`, writes `deploy/.env`, pulls the published image, starts everything and prints the setup token. Nothing else runs by hand afterwards. Everything else happens in the browser.

To check an install, run `docker compose -f deploy/docker-compose.yml exec app tether doctor`. It checks DNS, ports, TLS, the database, EVE login and Discord, and says how to fix anything that's off.

## First-run setup

Open your domain in a browser. The sign-in page links to the setup wizard, or go to `/setup`. The wizard has four steps.

1. **Setup token.** Enter the token install.sh printed. It's `SETUP_TOKEN` in `deploy/.env`, and it's also in the app's startup logs. It proves you run the server.
2. **EVE application.** Create an application at [developers.eveonline.com](https://developers.eveonline.com/applications). Choose Authentication & API Access, enable every scope the page lists, and set the callback URL to exactly the one the page shows. **Check this address reaches Tether** tests the callback. Paste the client ID and press **Save client ID**.
3. **First superuser.** Press **Log in with EVE Online** and log in with your main. That account becomes the first superuser. You can add alts afterwards.
4. **Member alliance.** Pick the alliance or corporation whose pilots get Member. Tether suggests your main's; **Make member** picks it. You can also find another by its exact name.

When setup is complete, the wizard offers to **Name this site**. The name shows in browser tabs and on the sign-in page. The sign-in page is public, so anyone who opens it sees the name. You can change it later on Settings.

Once a superuser exists, the setup token stops working.

Apps you install later may need more scopes on your EVE application. **States** lists every scope to enable under **Scopes on your EVE application**, and each app's page lists its own under **ESI access**. EVE refuses a login that asks for a scope your application doesn't allow, so enable them before pilots need them.

A good order for the rest:

1. States: who is Member, Blue and anything else.
2. Permissions: what each state may do.
3. Discord, if you use it.
4. Apps: install the ones you want and grant their permissions.
5. Groups, as you need them.

## States

A pilot's state comes from their main character. **States** (Administration › Access) lists them from the top. The first state that is public, or covers the main's alliance, corporation or the character itself, wins. Anyone no state covers is Guest. Changes apply to everyone within a minute.

Tether starts with three states, as Alliance Auth does: **Member**, **Blue** and **Guest**. Blue is a list you keep by hand, not standings. Member and Blue can be renamed and deleted. Guest can't: it covers nobody by name and is always last. A blacklisted pilot's account is in the Blacklist state, above every other.

On each state you can:

- **Add to it.** Type an exact name under **Add to <state>: exact name** and press **Find**, then **Add to <state>** on the result. Alliances, corporations, characters and factions all work. Remove one with the × beside it.
- **Set the priority.** States are checked from the highest priority down. Type a number and press **Set**.
- **Make it public.** A public state covers any character with a main, unless a state above covers them. It can't hold a sensitive permission.
- **Rename** or **Delete** it. Deleting a state moves its accounts to the next state that covers them, and its permissions and Discord roles go.
- **Require scopes.** Choose under **Also require a scope…** and press **Require**. Every character on an account in this state must then grant it.
- **Require an app.** **Require <app>'s scopes for <state>** makes every character in the state register for that app, as Alliance Auth's Member Audit compliance groups do.

**Add a state** at the bottom makes a new one. New states start just above Guest, so they only cover pilots no other state does until you move them up.

A change that moves accounts between states shows a **Confirm change** page first: who moves from where to where. Press **Apply** to do it.

### Compliance

Each state other than Guest can require scopes. Member always requires the corporation member list scope. An account with a character that hasn't granted what its state requires keeps its state, but is flagged. Its pilot sees a banner asking them to register, and the account leaves any compliance groups until every character is registered. **Compliance Report** (Administration › Members) lists who is flagged and what's missing.

## Groups

Groups add access on top of states. **Groups** (Administration › Access) lists them all. **New group** makes one: give it a name and description and press **Create group**. New groups start Internal and Hidden, as in Alliance Auth: untick Internal to let pilots find and request it.

Open a group with **Manage** to change it.

**Settings** sets its flags and states:

- **Internal**: pilots can't see, join or leave it; only admins change its members. Overrides the rest.
- **Hidden**: not listed on the Groups page; pilots join through its direct link.
- **Open**: pilots join and leave at once, without approval.
- **Public**: pilots who can't request groups may still join.
- **Restricted**: only a superuser adds or removes members on the admin pages or changes this setting; its leaders still handle requests.
- **States**: only these states may be in the group. None ticked means every state. Members whose state you untick leave the group when you save.
- **Compliance group**: Tether keeps its members, every compliant pilot in its states (never Guest). Internal groups only.

Press **Save settings** when done.

**Group Leaders** accept and reject requests, and remove members, in Group Management. Add a leader by character name (**Add leader**), or make every member of another group a leader (**Add leader group**). Leaders lead only groups that aren't Internal.

**Members** lists who is in the group. **Add member** takes any of a pilot's character names; the whole account joins.

To ask to join a group that isn't Public, a pilot needs **Can request non-public groups**. Member holds it on a new instance. A group nobody may ask to join says so on its page, with the fix.

Under the list of groups:

- **Group Management settings**: **Leave without approval** lets pilots leave any group at once, instead of asking its leaders. **Notify leaders of requests** sends leaders a notification for each join or leave request. Both are off by default.
- **Reserved names**: names no group can take, ignoring case. Discord leaves roles with these names alone.

### Group Management

Leaders, and holders of the Group Management permission, handle requests on **Group Management** in the sidebar. **Group Requests** lists join and leave requests with **Accept** and **Reject**. **Group Membership** lists the groups they lead, with members, the group's direct join link and its Audit Log.

### Secure Groups

A Secure Group is a group whose members Tether keeps by filters, as in allianceauth-secure-groups. Tick **Secure Group** when you create the group, or use the **Secure Group** section on its page.

- **Filters**: a state, a main or any character in a corporation or alliance, other groups, a militia, a main's age, being compliant, being linked to Discord, and filters apps add (such as Member Audit's skills and assets, or FATs from Fleet Activity Tracking). Every filter must pass; a **Reversed** one must fail. Two filters can be combined into one with AND, OR or XOR.
- **Auto group: add everyone who passes** adds pilots by itself. Without it, pilots who pass ask to join from the Groups or Secure Groups page.
- **Can grace** gives members who stop passing a grace period before they leave. Each filter has its own grace in days; new filters get 5.
- **Notify members** on add, on remove and on grace.
- **Post each run's summary to** one of the ping channels set on the Discord page.
- **Check now** runs the group's check at once. Otherwise Tether checks every hour.
- **Check a pilot** shows which filters pass or fail for one pilot now. It changes nothing, and it's in the audit log.

Pilots need **securegroups.access_sec_group** to open the Secure Groups page; nobody holds it on a new instance. Officers with **securegroups.audit_sec_group** see **Secure Group Audit**: every member of the Secure Groups they manage against each requirement.

An app's filter lets that app's answers decide who is in the group. Keep app filters off groups that grant admin permissions.

### Auto Groups

**Auto Groups** keeps a group for every main's corporation and alliance in the states you choose, as pilots move. Choose the states, a prefix, whether groups are named by name or ticker, and what replaces spaces. **Add config** saves it. The groups are Internal: grant them permissions or Discord roles like any other. New groups appear within the hour.

## Permissions

Permissions are granted to states, groups and single users, as in Alliance Auth. Superusers always have everything.

**Permissions** (Administration › Access) lists every permission, Tether's and each app's, by area. Each says what it allows and who it's usually for. **Edit** on a permission ticks the states and groups that hold it; **Save** changes only what you changed. The filter box at the top finds a permission by name.

To grant a permission to one user, open them under **Users**. **Granted to this user** lists their own grants, beside their state's and groups'. Pick a permission and press **Grant**.

A few rules hold everywhere:

- You grant or revoke only permissions you hold yourself, and never to yourself.
- Sensitive permissions (admin powers, among others) never go to Guest, a public state or an Open group, because anyone can be in those.
- Granting or revoking a sensitive permission needs a fresh EVE login.
- States aren't checked that way: changing who a state covers moves accounts in and out of its grants without a fresh login. Keep sensitive permissions on groups rather than states.

**Permissions Audit** shows every permission with how many states, groups and users hold it, how many accounts that adds up to, and who. Deactivated accounts hold nothing, and group permissions count only while an account has a main.

**Making superusers.** On a user's page under **Users**, **Make superuser** and **Revoke superuser** do this, with a fresh EVE login. There can be any number of superusers. The last one can't be revoked.

## Discord

Tether runs one Discord bot for the whole instance. Pilots with Discord access link their Discord account on the **Services** page and join the server with the roles for their state and groups. Tether only talks to Discord's REST API; nothing runs in the server.

Set it up on **Discord** (Administration › Integrations):

1. **Bot settings.** Create an application at [discord.com/developers/applications](https://discord.com/developers/applications). Copy its **Application ID** (General Information). Under OAuth2, copy the **Client Secret** and add the redirect the page shows, exactly. Under Bot, reset and copy the token; no privileged intents are needed. Turn on Developer Mode in Discord and copy your server's **Server ID**. Enter all four and press **Save and check**. The secret and token are stored encrypted and never shown again.
2. **Add the bot to the server.** The link asks for the permissions the bot needs: Create Instant Invite (to add members), Kick Members (members who lose access leave the server), Manage Nicknames and Manage Roles. If any is missing later, the page says which.
3. **Grant access.** Nobody may link Discord on a new instance. Grant **Can access the Discord service** (`discord.access_discord`) to Member, or the states and groups it's for, on Permissions.
4. **Roles.** Under **Map a role**, pick a Discord role and give it to a state or group, then press **Map role**. Tether gives mapped roles when pilots link and keeps them in step as states and groups change. Roles it doesn't manage are left alone. The bot can only give roles below its own. Tether never gives out Administrator, and roles with moderation or server-management permissions never go to Guest or Open groups.

**Service settings**:

- **Set nicknames**: every linked member's server nickname comes from the Name Formatter. Off by default.
- **Remove unmapped roles**: linked members lose every role not mapped to them, except Discord's own roles and roles named after a reserved group name.

The **Name Formatter** sets one nickname format per state, filled from each member's main, such as `[{corp_ticker}] {character_name}`. Empty uses `{character_name}`. The page lists the fields.

When a pilot loses Discord access (a state change, a permission, deactivation), the bot removes them from the server and unlinks them, and they're told. A member who leaves the server is unlinked too. Everyone linked is synced again every five minutes.

### Ping channels and Fleet Pings

**Ping channels**, on the Discord page, are where fleet pings may go. The bot needs View Channel and Send Messages there, and Mention Everyone for @everyone and @here. With no ping channel, fleet pings are off.

**Fleet Pings** (Administration › Integrations) sets what the ping form offers: channels, ping targets, fleet types, doctrines, formup locations and comms, and who may use which. Anything without a limit is open to everyone who can send pings. **Use doctrines from Fittings** offers the doctrines the Fittings app shares instead of your own list. Pilots need **fleetpings.basic_access** to send pings; on a new instance only superusers hold it.

## Apps

Apps add features without a restart: Moon Mining, Member Audit, Structures and the rest. Each runs sandboxed and gets only what you approve. Apps never see EVE tokens or Discord secrets.

**Apps** (Administration › Integrations) lists:

- **Installed** apps, with their version, status and **Manage**.
- **Included with Tether**: the first-party apps that come in Tether's image. They need no signing.
- **Install from GitHub**: apps other people publish.
- **Pinned keys**: the publisher key each app's packages must be signed with.

### Installing

For an app included with Tether, press **Review and install**. For an app from GitHub, enter its **Repository** (`https://github.com/owner/name`), and its **App id** only if the repository publishes several, then press **Fetch and check**. Tether fetches the newest release, checks its signature and shows the same review.

The review page shows:

- **What it asks for**: what the app can do beyond showing its own pages, such as reading ESI, posting to Discord, or reaching an outside website. Nothing else is possible for it.
- **Permissions it adds**: you grant these to states and groups like Tether's own.
- **Who may open its pages**.
- **Package**: its id, version, source and publisher key.

Press **Approve and install** to install it, or **Discard**. Approving needs a fresh EVE login. The first package installed under an app id pins its publisher key; every later version must be signed with the same key.

After installing, nobody may use the app until you grant its permissions on **Permissions**. Until then only superusers see it. The app's page says so, and each permission says who it's usually for.

### An app's page

Open an installed app with **Manage** on the Apps page.

- **Settings** opens the app's own settings, for apps that have them.
- **Disable** stops it without losing anything; **Enable** starts it again.
- **ESI access** lists the scopes to enable on your EVE application so pilots can grant them.
- **Data sources** and **Activity** (also under the app's own **Manage** button): its data sources, and its ESI and HTTPS calls, schedules, jobs and log. **Run now** runs a schedule at once.
- **Discord channels**: where the app may post. A channel must be a ping channel on the Discord page first. Apps can ping only the roles mapped to states, never @everyone or @here.
- **HTTPS hosts**: the outside hosts you approved for it. It can reach nothing else. Some apps need a secret, such as an API key: type its value and press **Save**. Setting secrets needs a fresh EVE login. Values are stored encrypted and never shown again, not even to the app.

### Data sources

Some apps read corporation data from ESI, such as moon extractions or structures. They do it through a **data source**: a character that holds the in-game role CCP requires (Station Manager or Director, for example), added by someone the app allows. As in Alliance Auth, there's no approval step: whoever holds the app's add permission adds their own character with one EVE login, and it's in use at once.

Each app's **Data sources** page (under its Manage) lists its sources, how each is doing, and **Add data source**. For app admins it also shows which Member corporations have a working source, and a link to send a Director of a corporation without one. **Withdraw** and **Remove** stop a source.

**Data sources** under Administration lists every app's sources in one table. The foot of the sidebar shows how many work. When one stops working (the character lost its role, or its login), the app's pages show a notice to whoever looks after it.

### Updates

Apps included with Tether update with Tether. A newer version that asks for nothing new is applied when Tether starts, unless you rolled back from it. The rest wait on the Apps page for your review: **Review update**, or **Review rebuild** for the same version rebuilt. When several wait, **Approve all included updates** shows them on one page and approves each in turn.

Apps from GitHub are checked once a day while update checks are on (Settings). The app's page shows the newest published version; **Review version** fetches it into an upgrade review. Leave **Repository** empty on the app's page to stop looking for updates.

An upgrade review shows what the new version asks for beyond the old one, and what it no longer asks for. If it has new database migrations, Tether takes a snapshot of the app's data first. **Approve and upgrade** installs it.

### Rolling back and uninstalling

After an upgrade, the app's page offers **Roll back to <version>**. It puts the earlier version back as it was approved. If the upgrade changed the app's data, the data goes back to the snapshot taken before it, and anything stored since is lost. Type the app's id to confirm. It's one step only.

**Uninstall** stops the app and deletes its data: its database schema and everything in it. Type the app's id to confirm. Its pinned key stays. Uninstalling doesn't fix an app that won't load: install a version that does instead, and its data stays.

If an app's publisher lost their key, **Details** under Pinned keys can replace it. Confirm the new key with the publisher somewhere other than where you got the package: whoever holds it can publish anything under that app id.

## Members and officer pages

- **Users**: every account, found by any of its characters. Open one to see its characters, state, groups and permissions; to **Deactivate** or **Reactivate** it; to remove a character from it; to grant permissions to that user alone; or to make them a superuser. Deactivating an account sends it to Guest, ends its sessions and refuses its sign-in.
- **Blacklist**: the Pilot Log keeps notes on pilots, corporations and alliances, and a note can blacklist them. An account whose main is, or is in, a blacklisted pilot, corporation or alliance is in the Blacklist state.
- **Compliance Report**: accounts whose characters aren't all registered with their state's scopes.
- **Corporation Stats**: each covered corporation's members, mains and who never registered, read daily with any registered Member character in it. **Update Now** reads one again.
- **Audit log** (under Instance): every admin action and sign-in change, newest first. Entries can't be edited or deleted.

## Settings

**Settings** (Administration › Instance) is one form, saved with the bar at the bottom of the page:

- **Site name**: shown in browser tabs and on the sign-in page.
- **Appearance**: the **Accent colour**, which marks the one thing that matters most on each screen.
- **Notifications**: how many notifications each user keeps (**Per user**; when one more arrives, the oldest goes), and **Tell pilots when Member Audit can't read a character**.
- **Update checks**: **Check for updates** lets Tether ask GitHub once a day whether a newer release exists. The request names Tether's version, never your instance. App update checks follow the same switch.

**Menu**, beside it, arranges the sidebar: rename, reorder and hide items, group them into sections and folders, and add your own links. Everyone still sees only the pages they may open.

## Health

**Health** (Administration › Instance) shows whether each part of Tether is working, read as the page loaded:

- **Systems**: ESI, ESI's limits, Discord, the job queue, backups, data sources and updates, each with its status.
- **ESI requests** and **Rate-limit groups**: how Tether is using ESI's budget.
- **Job queue**: jobs that gave up after retrying, with why, and **Retry**.
- **Schedules**: Tether's background work, with **Run now**. A schedule runs by hand at most once a minute.
- **Version**: what's running, whether a newer release exists, **Check now**, and the upgrade and rollback buttons (below).

When a newer release exists, the sidebar foot says **UPDATE AVAILABLE**.

## Backups and rollback

Tether keeps encrypted copies of its database in the `snapshots` volume:

- **Snapshots**, taken before any database migration, whether Tether's own (at startup, after an upgrade) or an app's (before its upgrade). The last five of each kind are kept.
- **Nightly backups** of Tether's data and every app's. A week of them is kept. Health's Backups line says when the last one ran.

They are encrypted with `ENCRYPTION_KEY` from `deploy/.env`. Without that key they can't be restored, so keep a copy of the key somewhere apart from the server. Tether doesn't copy backups off the server yet; copy the `snapshots` volume elsewhere if you need that.

Rolling back:

- **Tether itself**: the console's **Roll back** (below) goes one step back, restoring the snapshot the upgrade took. On the server, `tether rollback` does the same by hand; [deploy/README.md](../../deploy/README.md), Rolling back, has the steps. `docker compose -f deploy/docker-compose.yml exec app tether rollback --list` lists the snapshots and backups.
- **An app**: **Roll back to <version>** on the app's page (see Apps).

A rollback puts everything of that kind back as it was in the snapshot, and anything written since is lost. Rolling back Tether's own data also brings back permissions, tokens and blacklist entries removed since.

## Upgrades

From the console: on **Health**, the **Version** card offers **Upgrade to <version>** once the daily check has seen a newer release (on `edge`, **Update to the newest edge**). Tether's updater pulls the new version and restarts the app, which takes a snapshot and migrates first. The page follows its progress. It needs a fresh EVE login.

The same card offers **Roll back to <version>**, one step back, restoring the snapshot that upgrade took. Type the version to confirm.

When a newer release exists, the Version card links to its release notes. After an upgrade, every pilot sees a **What's new** popup with the changes that concern them; app admins also see which apps changed version.

On the server, `deploy/install.sh --version X.Y.Z` does the same as the console's upgrade. [deploy/README.md](../../deploy/README.md) covers both, and what to do when the updater isn't running.

## The command line

A few things are done on the server, with `docker compose -f deploy/docker-compose.yml exec app tether <command>`:

- `doctor`: checks the install and says how to fix what's off.
- `users`: lists accounts. `users show <name>`, `users deactivate <name>`, `users reactivate <name>`, and `users main <account> <character>` sets the main of an account that lost its own (only the main signs in).
- `states`: shows the states; `states add <state> <EVE id>` and `states remove <state> <EVE id>` change what one covers.
- `jobs`: the job queue. `jobs retry <id>` retries a dead job, `jobs schedules` lists the schedules, and `jobs run <schedule>` runs one now (`jobs run plugin:<app id>:*` runs all of an app's).
- `sync`: queues an affiliation sync for every character.

`rollback` is the exception: it runs with the app stopped, as `docker compose run --rm app rollback` from the `deploy` directory ([deploy/README.md](../../deploy/README.md), Rolling back).
