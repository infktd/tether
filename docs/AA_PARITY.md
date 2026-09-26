# Alliance Auth parity

Tether is Alliance Auth v2: the same model, admin and audit features, rebuilt in Rust with better UI, UX, security and deployment. This file lists every feature of AA core, its bundled apps and the community apps alliances rely on, under AA's own names, next to where Tether stands. It was checked against AA v5.4.0 (released 2026-09-24) and current app releases on 2026-09-25.

**Rules**

- **Same names.** Where AA has a name for something, Tether uses it: in the UI, the docs, the PRD and the code where practical. The rename list is below.
- **Same behaviour.** Match AA's rules; improve the UX. Where Tether differs on purpose, it's listed under "Deliberately different".
- **Core or plugin.** Admin and audit features, including the AA apps alliances use for administration (Corporation Stats, compliance, Auto Groups, Permissions Audit), are Tether core. Feature apps (Member Audit, Moon Mining, SRP, timers) are plugins.

Status: **done**, **partial** (exists, but short of AA), **planned** (already a PRD item), **missing** (proposed below), **skip** (with a reason).

## Names

| AA name | Tether today | Change |
| --- | --- | --- |
| Dashboard (the landing page: Characters, Membership) | Profile | Rename to **Dashboard** |
| Characters, Add Character, Change Main, Main Character | Characters, Add character, Make main, Main character | **Change Main** for "Make main"; drop the "Alt" label (AA just lists Characters) |
| State; Member, Blue, Guest | same | none |
| Groups → Available Groups (users) | **Groups** page and direct join links | done |
| Group Management → Group Requests, Group Membership, Audit Log | **Group Management** (leaders and `group_management`); group settings under Admin → Groups | done |
| Internal, Hidden, Open, Public, Restricted; Requestable | same | done |
| Group Leaders, Group Leader Groups | same | done |
| Permissions; Permissions Audit | Permissions; **Permissions Audit** | done |
| Token Management | **Token Management** | done |
| Notifications | same | done |
| Services, Services Management; "Can access the Discord service" | **Services** page; `discord.access_discord` | done |
| Name format config, `{character_name}`, `{corp_ticker}` ... | **Name Formatter**, AA's fields | done |
| Corporation Stats: Mains, Members, Unregistered, Update Now | **Corporation Stats** | done |
| Member Audit → Reports → User Compliance; Compliance Groups; Register Character | Compliance page, Compliant group, Register your characters | **Compliance Report**, **Compliance Group**, **Register Character** |
| Auto Groups | **Auto Groups** (Admin) | done |
| Fleet Pings (aa-fleetpings) | Pings | Rename to **Fleet Pings** |
| Moon Mining (aa-moonmining): Moons, Extractions, Reports, Add Corporation | Moon Tracker (planned plugin) | Rename the plugin to **Moon Mining**, with AA's pages |
| Member Audit: My Characters, Character Sheet, Character Finder, Reports, Skill Sets | Member Audit plugin, same names | done |
| Community Apps / apps | Plugins | **Apps** in the UI; "plugin" in the SDK and code |
| Add Refinery Owner, Add Structure Owner (apps' corp data characters) | data sources | Show as **owners** ("Add Owner"); keep "data source" in the SDK |

## Core

| AA feature | What it does in AA | Tether | Status |
| --- | --- | --- | --- |
| Sign In with EVE SSO | `publicData` only; main character only | same, no scopes at login | done |
| Email step | asks for and verifies an email | none | skip: no email, ever (PRD non-goal) |
| Characters, Add Character | alts join the account through SSO | same | done |
| Change Main | switch the main; state re-evaluated | picks from linked characters | partial: valid token required (see Behaviour) |
| Character ownership check (every 4 h) | owner hash re-checked; sold characters removed | at each login, on transfer, and every 4 hours (`ownership.check` refreshes each token, as AA) | done |
| States | Name, Permissions, Priority, Member Characters/Corporations/Alliances/**Factions**, **Public** | all, factions included (the main's militia, from ESI's affiliation endpoint); Guest covers everyone, which is what Public is for | done |
| State changes | re-evaluated on affiliation updates; "State changed to: X" notification | same, and audited | done |
| Groups: Internal, Hidden (direct join link), Open, Public, Restricted, States (only these states may join; removed on state change), Description | | same | done |
| Group Leaders, Group Leader Groups | non-admins process one group's requests | same; accepting also needs the group's permissions | done, stricter |
| Leave requests, `GROUPMANAGEMENT_AUTO_LEAVE` | leaving non-open groups needs approval unless auto-leave is on | same (setting off by default) | done |
| Group Management: Group Requests (Join/Leave, Accept/Reject), Group Membership (View Members, Audit Members, Copy Direct Join Link) | | same | done |
| Group Audit Log (RequestLog) | per group: requestor, character, corporation, type, action, actor | same, plus the global audit log | done |
| Reserved group names | names groups can't use; matching Discord roles left alone | same (Discord's strip-unmapped option leaves them) | done |
| `request_groups` permission | who may request non-public groups (usually via Member) | same, granted to Member | done |
| Permissions on users, groups, states | | states and groups only (never users, F6) | done, deliberately stricter |
| Staff can't change permissions; superuser can | | `admin.permissions`; owner holds everything | done |
| Notifications | in-app, unread count, mark all read, delete read, max per user | same; the count is live | done |
| Token Management | list tokens and scopes, delete, refresh | same; a refresh runs the ownership check, and a deleted token follows the dead-token rules (the character leaves a day later unless logged in again) | done |
| Dashboard | Characters and Membership widgets; admin panels: Software Version, Task Queue, ESI status, Announcements; apps add widgets | **Dashboard**: summary (state, characters, groups, permissions) and characters; for `admin.system`, System panels (Software Version, Task Queue, ESI, error budget); apps add up to 3 `[[widgets]]` each (a page's sections, shown to whoever may open the page) | done, except Announcements (Fleet Pings and Discord cover them) |
| Admin site | Django admin for every model | purpose-built admin pages, API and CLI; **Users** (Admin → Users): find any account by any character (or id), filter by state and status, see its characters (corporation, token, added, last login), state, groups and permissions, Deactivate or Reactivate | done, deliberately different |
| Menu (reorder, hide, folders, custom links) | | **Menu** (Admin → System → Menu): rename, reorder and hide sidebar items, sections and one level of folders, custom links (https or a page here, optionally in a new tab), reset; apps' links included, and new pages and apps appear in their usual section | done; hiding only hides (who may open a page is still permissions) |
| Themes, Custom CSS | | dark theme; the accent colour (Admin → System → Appearance) | done; no custom CSS (plugins never ship CSS) |
| Analytics | opt-out telemetry to Google Analytics | none | skip: no telemetry (N5) |
| Services framework | per-service access permission; access removed when the permission goes | same, for Discord | done |
| Discord | Link Discord Server, roles mirror groups, nickname sync, kicked on losing access | explicit role mapping (plus an option to strip unmapped roles), nickname sync, kicked on losing access or unlinking, fleet pings | done (mapping deliberately explicit) |
| Name Formatter | one format per service per state; AA's field list | one format per state (Discord), AA's fields and format specs | done |
| Mumble, TeamSpeak 3, Openfire/Jabber, phpBB3, SMF, IPS4, XenForo, Discourse | | none | after launch |
| Periodic tasks: affiliation update, token cleanup | | hourly affiliation sync, daily token check | done |

## Bundled AA apps

| AA app | Tether | Status |
| --- | --- | --- |
| Auto Groups | Admin → Auto Groups: per-state configs, corporation and alliance groups, prefix, name or ticker, space replacement; groups are Internal and kept by Tether; a config's groups go with it | done; stricter than AA, it never takes over an existing group of the same name |
| Corporation Stats (Mains, Members, Unregistered, Search, Update Now; view_corp/alliance/state permissions) | Corporation Stats: Mains, Members and Unregistered tabs, search, Update Now, AA's three view permissions (compliance.view sees all); sources offered by members and approved by admins | done |
| Permissions Audit (`permissions_tool.audit_permissions`) | Admin → Permissions Audit: every permission (core and apps') with counts of states, groups and active accounts holding it; each one lists its holders and whether the owner, their state or which groups give it | done; counts follow Tether's rules (the owner holds everything, deactivated accounts nothing, groups only with a main) |
| Fleet Activity Tracking | Fleet Activity Tracking plugin (`plugins/fleet-activity-tracking`), with aa-afat's pages and permissions (`basic_access`, `add_fatlink`, `manage_afat`, `stats_corporation_own`, `stats_corporation_other`, `logs_view`): **Dashboard** (your recent FATs, open and recent FAT links), **FAT Links**, **Create FAT Link** (fleet name, fleet type, doctrine, expiry), the link's page (register link, attendees, edit, close, reopen once or by a manager, manual FAT add, and for managers FAT removal and delete), members' register page (every character they brought, once each, only while the link is open), **Statistics** by month for your characters, your corporation, every corporation and alliance, with per-pilot, per-corporation and per-fleet-type breakdowns, **Fleet types** (admin-managed) and **Logs** (60 days) | partial: ESI-tracked fleets planned, once core offers a fleet endpoint with FC opt-in (aa-afat asks only FCs for `esi-fleets.read_fleet.v1`; a plugin user scope would make Member require it of every character) |
| Fleet Operations (optimer) | Fleet Ops plugin (F22) | planned |
| Structure Timers (timerboard: structure, timer type, objective, system, planet/moon, EVE time, details, important, corporation timers; `timer_view`, `timer_management`) | Structure Timers plugin (`plugins/structure-timers`): Upcoming and Past with countdowns, Create Timer from the EVE time or the time left, Edit Timer and delete; corporation timers seen by the creator's corporation only | done; stricter than AA: a corporation timer can't be edited or deleted from outside its corporation. Countdowns are drawn when the page loads (plugin pages have no live updates) |
| Ship Replacement (SRP) | none | planned: first-party plugin |
| HR Applications (a form per corporation with questions, written or multiple choice; My Applications; HR Application Management with Pending and Reviewed, search, Mark in Progress, comments, approve, reject, delete; `human_resources`, `approve_application`, `reject_application`, `delete_application`) | HR Applications plugin (`plugins/hr-applications`): the same, with AA's names; Application Forms in the plugin (AA's admin site; `manage`); `all_corporations` for what AA's superusers see | done; stricter than AA: nobody reviews their own application, an applicant can withdraw only until a reviewer marks it in progress (so reviewers' comments survive), and applying asks the pilot's consent to share answers and characters. Comments are capped at 200 per application. The characters shown are those on the account when they applied (plugins see an account only while its owner is looking) |

## Community apps

| App | Tether | Status |
| --- | --- | --- |
| Member Audit (My Characters, Character Sheet, Character Finder, Reports: User Compliance, Corporation Compliance, Skill Sets; Compliance Groups; sharing) | compliance, Compliance Groups and Register Character are core; the Member Audit plugin (`plugins/member-audit`) has My Characters (with combined totals and queues for multiboxers), the Character Sheet (skills, queue, assets, wallet journal, clones, implants, location, ship), Character Finder, Skill Sets and the Skill Sets report | done, except mail, contacts, contracts, loyalty, planets and character sharing (more scopes every Member would need; on request) |
| Moon Mining (Moons, Extractions, Reports, Add Corporation, surveys, ledgers) | Moon Mining plugin (`plugins/moon-mining`): extractions, pop pings, fresh moons for Members and old ones for Blue, mining totals by pilot and ore, and an extraction planner for Station Managers; corporations come from approved data sources (AA's Add Corporation) | done, except moon surveys (ore composition) and ISK values, which need prices |
| Structures (fuel, notifications to Discord, timers) | none | planned: first-party plugin (PRD), Jay's call for the NMU release |
| Fleet Pings | **Fleet Pings**: aa-fleetpings' fields (pre-ping, fleet type, FC, fleet name, formup location and time, comms, doctrine with its link, SRP, additional information), a Discord embed coloured by fleet type, copy-paste text; Fleet Pings settings: fleet types, doctrines, formup locations, comms, and channels, pings, fleet types and doctrines limited to states or groups; @here and @everyone can be switched off | done; the bot posts (no webhooks), so the ping history and retries stay in Tether |
| Secure Groups (Smart Groups, filters, grace) | the Compliance Group is one fixed filter | planned: core, with app-provided filters (PRD) |
| AA SRP, AFAT | AFAT's additions (alliance stats, fleet types, expiring links, logs) are in the Fleet Activity Tracking plugin | SRP: planned plugin; AFAT: partial, ESI-tracked fleets planned (above) |
| AA-Discordbot | none | skip: Tether's Discord is REST only |
| Blacklist ("Pilot Log", Blacklist state) | **Blacklist** (/blacklist): blacklist pilots, corporations or alliances with a reason; every account with a character that is, or is in, a listed entry goes to the Blacklist state at once (any character, not just the main, so a clean alt is no way out), holding nothing (no permissions, groups, group leadership or services) until removed; never the owner or an NPC corporation, and never an account holding permissions the admin doesn't. **Pilot Log**: notes on anyone, shown on the Users page. Permissions `blacklist.view_blacklist`, `blacklist.add_notes`, `blacklist.manage_blacklist` | done |
| Corp Tools, CorpStats 2.0 | overlap with Member Audit and Corporation Stats | skip |

## Behaviour

Audited against AA v5.4.0's source (and aa-memberaudit 5.2.0, aa-fleetpings 4.1.1) on 2026-09-25, rule by rule against Tether's code. Where AA's docs and code disagree, Tether follows the docs (what AA intends). Decisions from Jay are marked **(decided)**.

**Login, characters and ownership**

- **Only the main signs in.** Signing in with an alt is refused: "Unable to authenticate as the selected character. Please log in with the main character associated with this account." The token isn't stored. Alts join only through **Add Character**; a plain login while signed in switches to (or refuses) that character's account, never links it. *Tether today: any linked character signs in, and a plain login while signed in adds an alt.*
- **A character linked to another account moves** to the account that just added it with a fresh SSO login; the move is audited and the other account re-evaluated. **(decided)** *Tether today: refused.*
- **Ownership follows the owner hash.** A changed owner hash (the character was sold) is caught at login, on every token refresh (the refreshed token's `owner` is compared), and by an ownership check every 4 hours covering every token, scopes or not. Every change of owner is kept as an **ownership record**; a returning owner (same hash) is re-attached to their old account. *Tether today: caught only at login.*
- **Losing the main clears it.** When the main is sold or loses its last valid token, the account has no main: Guest, services off, until the owner picks one. An alt that loses its last valid token leaves the account. Nothing is promoted silently. Deliberately different: AA refuses every sign-in to an account without a main (its owner is stuck unless still signed in); Tether lets the owner sign in with one of the account's characters, which SSO just proved, and makes it the main, the same way AA re-attaches a returning owner. If the owner account loses its last character, it stops being the owner and first-run setup reopens behind the setup token.
- **Safety on top of AA** (deliberate): only `invalid_grant` / `invalid_token` count as a dead token (errors about the whole app, such as a wrong client id, never do); a character leaves its account for a dead token only after a day's grace, if it's still dead then, and never the owner account's last character; if a tenth or more of all tokens die at once, nobody loses anything until an admin looks (a sale proven by a changed owner hash still acts at once). Deactivating someone needs every permission they hold, and a deactivated account's characters can't be moved to a fresh account.
- **Change Main** only to a character with a valid token.
- **Deactivate account** (admin, audited; not the owner): Guest, sessions ended, login refused, services removed; reactivate undoes it. **(decided)**
- **Sessions** last 14 days from sign-in (Django's default), rotated at sign-in. *Tether today: 30 days.*
- **Names** refresh with the affiliation sync, so nicknames follow renames. *Tether today: at login only.*

**States**

- Defaults Member 100, Blue 50, Guest 0 (priorities are editable numbers; the page keeps the up and down buttons). **Member and Blue can be renamed and deleted; only Guest is protected** (fixed name, always last, can't be deleted). **(decided)** Plugins and scopes follow the built-in role, not the name: deleting Member drops plugin scope requirements and plugins' Member characters; deleting Blue leaves Moon Mining's old-moon list with no audience. State names at most 32 characters.
- A state change removes the account from groups whose allowed states exclude it, re-checks services, and notifies ("State changed to: {state}" / "Your user's state is now: {state}", info). Deleting a state moves its accounts to their next state, notified the same way.

**Groups** (rules the Groups parity task implements)

1. Flags: Internal, Hidden, Open, Public, Restricted, and allowed states (empty: all). New groups default to Internal and Hidden, as in AA. Existing groups migrate: Assigned becomes Internal; Open becomes Open and not Hidden; Request to join becomes not Open, not Hidden. The Compliance Group is Internal.
2. Joinable: not Internal, and the account's state allowed. The Groups page lists joinable groups that aren't Hidden, where the account holds `request_groups` or the group is Public. Hidden groups join through their direct link.
3. Join, in order: not joinable (refused), already a member, no `request_groups` and not Public (refused), Open (added, logged Join/Accept by themselves), a pending request (refused), else a join request (approvers notified if the setting is on).
4. Leave, in order: Internal (refused), not a member, Open or auto-leave on (removed, logged Leave/Accept), a pending request (refused), else a leave request. Auto-leave is a setting, off by default.
5. Retract: join requests only; not logged.
6. Accepting a join re-checks joinability against the current state. The four decisions (join or leave, accept or reject) are logged, delete the request and notify the requester: "Group Application Accepted" / "Your application to {group} has been accepted." (success), "Group Application Rejected" / "Your application to {group} has been rejected." (danger), "Group Leave Request Accepted" / "Your request to leave {group} has been accepted." (success), "Group Leave Request Rejected" / "Your request to leave {group} has been rejected." (danger).
7. Removing a member (non-Internal groups) is logged as Removed, without a notification.
8. On a state change, and when a group's allowed states are saved, accounts whose state isn't allowed are removed (Public groups too).
9. **Group Management** covers non-Internal groups: holders of `group_management` all of them, **Group Leaders** (directly, or through a **Group Leader Group**) only theirs. They process requests, view and remove members, and read the group's Audit Log; they can't change settings or add members directly. The menu shows the pending count.
10. Request notifications ("Group Management: Join request for {group}" / "{user} wants to join {group}.", info; and Leave) go to the group's leaders and leader groups only, behind a setting that's off by default.
11. **Restricted**: only the owner changes the group's settings (the flag included), its leaders or its membership (leaders included, which is stricter than AA's code and what its docs intend). Joining an Open Restricted group is a request the owner accepts.
12. **Reserved group names**: matched ignoring case, with a required reason; groups can't take them; Discord leaves roles with those names alone.
13. The per-group **Audit Log**: date, requestor, current main, corporation, type (Join, Leave, Removed), action (Accept, Reject), actor, kept with name snapshots.
14. `request_groups` ("Can request non-public groups") gates listing and joining non-Public groups and is granted to Member by default (AA's docs).
15. Tether's own guards stay: adding members, accepting requests, appointing leaders, opening a group and making it a compliance group all need the group's grants (and those of the groups it leads); sensitive permissions never go to Guest or Open groups, re-checked when flags change. Stricter than AA: an Open or compliance group can't be a Group Leader Group, and a leader counts only while active, with a main, and not Guest. Hidden only takes a group off the list; it isn't access control.

**Compliance** (Member Audit's behaviour)

- **Compliance Groups**: admins mark Internal groups as compliance groups, each limited to its allowed states, as in Member Audit (several allowed, e.g. one per state). Replaces the single fixed Compliant group. Accounts are added and removed as compliance changes, and notified.
- Tether is deliberately stricter than Member Audit: a revoked token breaks compliance (Member Audit only asks to re-register).

**Notifications** (rules the Notifications task implements)

- Levels danger, warning, info, success; title at most 254 characters; message defaults to the title. At most 50 per account: the oldest (read or not) go first. Only the recipient sees them; opening one marks it read; delete one, mark all read, delete all read; the unread count updates live.
- Sent for: state changes; the group decisions and (opt-in) requests above; compliance gained or lost; Discord access removed ("Discord Account Disabled", warning); a character lost to a sale ("Character {name} biomassed" is AA's for deletions); a Corporation Stats source that stopped working (to its owner). Not for member removals, open or auto join and leave, or retracts.

**Services and Discord**

- **Losing access kicks.** When an account loses Discord access (state, permission, deactivation, a lost main, or its deletion), the bot removes the member from the server and unlinks them, and they're notified. The bot needs Kick Members. Unlinking by the user also leaves the server. If the bot may not kick someone (no Kick Members, or the server owner), it takes Tether's roles instead.
- Access is by a permission ("Can access the Discord service", granted to Member and Blue by default), re-checked on state, permission and group changes.
- **Roles: explicit mapping stays** (AA mirrors groups and states by name and creates roles; a same-named group can then hand out a privileged role). **(decided)** A setting adds AA's behaviour of removing every unmapped role except Discord-managed roles and reserved names **(decided)**, and (stricter than AA) moderation and admin roles, so one checkbox can't strip the server's staff.
- **Name Formatter**: one format per state; AA's fields (`character_name`, `character_id`, `corp_ticker`, `corp_name`, `corp_id`, `alliance_ticker`, `alliance_name`, `alliance_id`, `alliance_or_corp_name`, `alliance_or_corp_ticker`, `username`) with format specs such as `{character_name:.20}`; default `{character_name}`; 32 characters.
- The stored Discord username refreshes with the daily sync.

**Corporation Stats**

- Mains (accounts whose main is in the corporation, with their characters), Members (registered characters in it), Unregistered. View permissions per corporation, alliance or state (either grants access), plus the owner. **Update Now**, checking the viewer may see that corporation (AA doesn't).
- A source that stops working (token, left the corporation) notifies its owner.
- Kept from Tether: an admin approves each source, several sources per corporation as fallbacks, only covered corporations.

**Fleet Pings** (aa-fleetpings)

- Fields: ping target, pre-ping, channel, fleet type, FC, fleet name, formup location, formup time (or now), comms, doctrine (with link), SRP (with link), additional information, and copy-paste text as well as posting.
- Restrictions: each channel, target, fleet type and doctrine can be limited to groups or states, **checked on the server** (aa-fleetpings only filters the form); a setting turns off @here and @everyone.
- Kept from Tether: the bot posts (no webhooks), mentions in typed text are defused, pings are rate-limited and audited.

## Deliberately different

- No email, no telemetry, no CDNs; purpose-built admin pages instead of the Django admin.
- Permissions go to states and groups only, never to single users (F6).
- Compliance, Corporation Stats and the Compliance Group are core, not apps.
- Plugins never see tokens; data access is checked and logged per call.
- Discord only for v1; other services on request, as plugins.
- One token per character, holding every scope granted (AA keeps one per scope set); a failed token is kept as revoked and audited, not deleted, while its effect on the account matches AA's.
- Discord: explicit role mapping (above); OAuth state checked; the bot has only the permissions it needs, never Administrator; the member's Discord token is revoked after linking; members who leave the server stay linked (PRD).
- Corporation Stats sources need an admin's approval, and only covered corporations are read.
- Compliance is stricter than Member Audit: revoked tokens break it.
- Evaluations run on the job queue, not inline, so a burst of changes can't stall requests.

## Proposed PRD tasks

In order; each is one checklist item with tests, a security review where it applies, and one commit.

1. **AA names**: the renames above across UI, docs, PRD and code; permission names aligned (`group_management`, `request_groups`, `permissions_audit`, Corporation Stats' `view_corp`/`view_alliance`/`view_state`); migrations rename grants.
2. **Groups parity**: Internal, Hidden (direct join link), Open, Public, Restricted flags; allowed states (removed on state change); Group Leaders and Group Leader Groups; leave requests with the auto-leave setting; the users' **Groups** page; **Group Management** with Group Requests, Group Membership and per-group Audit Log; reserved group names; `request_groups`.
3. **Notifications**: in-app, unread count in the top bar (live over SSE), mark read, delete read, at most 50 per user; sent for state changes, group requests and decisions, compliance, token revocation.
4. **Token Management**: every token and its scopes, delete and refresh; the ownership check on the daily token run.
5. **Services and Name Formatter**: a Services page for users; Discord access by permission (granted to Member and Blue by default); one name format per state with AA's fields.
6. **Auto Groups**: automatic corporation and alliance groups for chosen states (prefix, name or ticker, spaces).
7. **Corporation Stats parity**: its own page with Mains, Members and Unregistered tabs, search, Update Now, and view permissions per corporation, alliance or state.
8. **Permissions Audit**: every permission with counts of states, groups and accounts, and who holds it.
9. **Dashboard**: rename Profile, admin panels (version, task queue, ESI status) and plugin widgets.
10. **States: Factions**: cover a faction (Faction Warfare) as AA does.
11. **Fleet Pings fields**: fleet type, formup location, comms, doctrine, as aa-fleetpings.
12. **Accent colour setting** from DESIGN.md.

After v1: Secure Groups (Smart Groups with filters and grace), Blacklist notes. After launch: services other than Discord.

## Decisions (2026-09-25)

1. **Apps, not plugins, in the UI.** Admins and users see "Apps" (Admin → Apps), as in AA. The SDK, WIT, manifest and code keep "plugin", the accurate technical term.
2. **Order.** Tasks 1 to 5 above come first, then the Moon Mining and Member Audit plugins, then tasks 6 to 12, then the rest of milestone 2. They are in `docs/PRD.md`.
3. **Other AA apps.** ~~NMU uses none beyond Member Audit and Moon Mining for now, so Fleet Activity Tracking, Structure Timers, Structures, SRP and HR Applications stay off the plan until asked.~~ Superseded by 4.
4. **Feature parity is 1:1 with AA core** (2026-09-25, Jay): AA's bundled apps (Fleet Activity Tracking, Structure Timers, Ship Replacement, HR Applications; Fleet Operations is F22) are built as first-party plugins, and Menu customization moves into milestone 2. Services other than Discord (Mumble, TeamSpeak 3, Openfire, the forums) come after launch: NMU only uses Discord. Community apps stay as listed.
