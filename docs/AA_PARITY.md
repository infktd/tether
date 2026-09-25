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
| Groups → Available Groups (users) | none: joining only through the API | Build **Groups** (Available Groups) |
| Group Management → Group Requests, Group Membership, Audit Log | Admin → Groups | Rename to **Group Management**, with those tabs |
| Internal, Hidden, Open, Public, Restricted; Requestable | Assigned, Open, Request to join | Use AA's flags (see Groups) |
| Group Leaders, Group Leader Groups | none | Build |
| Permissions; Permissions Audit | Permissions | Add **Permissions Audit** |
| Token Management | scopes shown per character on the profile | Build **Token Management** |
| Notifications | none | Build |
| Services, Services Management; "Can access the Discord service" | Discord (admin), Discord card on the profile | **Services** page for users; access by permission (see Services) |
| Name format config, `{character_name}`, `{corp_ticker}` ... | nickname template: `{name}`, `{corp}`, `{alliance}` | AA's **Name Formatter** fields, one format per state |
| Corporation Stats: Mains, Members, Unregistered, Update Now | Corp Stats, inside Compliance | Own page **Corporation Stats**, AA's tabs |
| Member Audit → Reports → User Compliance; Compliance Groups; Register Character | Compliance page, Compliant group, Register your characters | **Compliance Report**, **Compliance Group**, **Register Character** |
| Auto Groups | none | Build |
| Fleet Pings (aa-fleetpings) | Pings | Rename to **Fleet Pings** |
| Moon Mining (aa-moonmining): Moons, Extractions, Reports, Add Corporation | Moon Tracker (planned plugin) | Rename the plugin to **Moon Mining**, with AA's pages |
| Member Audit: My Characters, Character Sheet, Character Finder, Reports, Skill Sets | Member Audit (planned plugin) | Use AA's page names |
| Community Apps / apps | Plugins | **Apps** in the UI; "plugin" in the SDK and code |
| Add Refinery Owner, Add Structure Owner (apps' corp data characters) | data sources | Show as **owners** ("Add Owner"); keep "data source" in the SDK |

## Core

| AA feature | What it does in AA | Tether | Status |
| --- | --- | --- | --- |
| Sign In with EVE SSO | `publicData` only; main character only | same, no scopes at login | done |
| Email step | asks for and verifies an email | none | skip: no email, ever (PRD non-goal) |
| Characters, Add Character | alts join the account through SSO | same | done |
| Change Main | switch the main; state re-evaluated | same; picks from linked characters (no SSO round trip) | done; rename |
| Character ownership check (every 4 h) | owner hash re-checked; sold characters removed | checked at each login and on transfer | partial: add to the daily token check |
| States | Name, Permissions, Priority, Member Characters/Corporations/Alliances/**Factions**, **Public** | all but Factions and Public; Guest covers everyone, which is what Public is for | partial: add Factions |
| State changes | re-evaluated on affiliation updates; "State changed to: X" notification | re-evaluated; audited | partial: needs Notifications |
| Groups: Internal, Hidden (direct join link), Open, Public, Restricted, States (only these states may join; removed on state change), Description | | Assigned ≈ Internal; Open; Request to join ≈ Requestable | partial |
| Group Leaders, Group Leader Groups | non-admins process one group's requests | none | missing |
| Leave requests, `GROUPMANAGEMENT_AUTO_LEAVE` | leaving non-open groups needs approval unless auto-leave is on | leaving is immediate | missing (setting, default off as in AA) |
| Group Management: Group Requests (Join/Leave, Accept/Reject), Group Membership (View Members, Audit Members, Copy Direct Join Link) | | admin group page: members, requests | partial |
| Group Audit Log (RequestLog) | per group: requestor, character, corporation, type, action, actor | global audit log | partial: per-group view |
| Reserved group names | names groups can't use; matching Discord roles left alone | Discord roles Tether doesn't map are left alone | partial |
| `request_groups` permission | who may request non-public groups (usually via Member) | anyone signed in | missing |
| Permissions on users, groups, states | | states and groups only (never users, F6) | done, deliberately stricter |
| Staff can't change permissions; superuser can | | `admin.permissions`; owner holds everything | done |
| Notifications | in-app, unread count, mark all read, delete read, max per user | none | missing |
| Token Management | list tokens and scopes, delete, refresh | scopes per character on the profile | partial |
| Dashboard | Characters and Membership widgets; admin panels: Software Version, Task Queue, ESI status, Announcements; apps add widgets | Profile; admin System page (ESI health, job queue, error budget, updates) | partial: rename, admin panels, plugin widgets |
| Admin site | Django admin for every model | purpose-built admin pages, API and CLI | done, deliberately different |
| Menu (reorder, hide, folders, custom links) | | fixed sidebar | missing (after v1) |
| Themes, Custom CSS | | dark theme; accent colour setting designed but not built | partial: build the accent setting; no custom CSS (plugins never ship CSS) |
| Analytics | opt-out telemetry to Google Analytics | none | skip: no telemetry (N5) |
| Services framework | per-service access permission; access removed when the permission goes | Discord for any state but Guest | partial: access by permission |
| Discord | Link Discord Server, roles mirror groups, nickname sync, removed on losing access | same, plus state roles and fleet pings | done |
| Name Formatter | one format per service per state; AA's field list | one template, three fields | partial |
| Mumble, TeamSpeak 3, Openfire/Jabber, phpBB3, SMF, IPS4, XenForo, Discourse | | none | skip for v1: plugin candidates on request |
| Periodic tasks: affiliation update, token cleanup | | hourly affiliation sync, daily token check | done |

## Bundled AA apps

| AA app | Tether | Status |
| --- | --- | --- |
| Auto Groups | none | missing: core |
| Corporation Stats (Mains, Members, Unregistered, Search, Update Now; view_corp/alliance/state permissions) | Corp Stats: unregistered members per corporation, daily | partial: core |
| Permissions Audit | Permissions page lists grants | missing: core ("who has this permission") |
| Fleet Activity Tracking | none | not planned: NMU doesn't use it |
| Fleet Operations (optimer) | Fleet Ops plugin (F22) | planned |
| Structure Timers | none | not planned: NMU doesn't use it |
| Ship Replacement (SRP) | none | not planned: NMU doesn't use it |
| HR Applications | none | not planned: NMU doesn't use it |

## Community apps

| App | Tether | Status |
| --- | --- | --- |
| Member Audit (My Characters, Character Sheet, Character Finder, Reports: User Compliance, Corporation Compliance, Skill Sets; Compliance Groups; sharing) | compliance, the Compliance Group and Register Character are core; the rest is the Member Audit plugin (F20) | partial / planned |
| Moon Mining (Moons, Extractions, Reports, Add Corporation, surveys, ledgers) | Moon Tracker plugin (F21), plus a pop-time planner for Station Managers | planned; rename |
| Structures (fuel, notifications to Discord, timers) | none | not planned: NMU doesn't use it |
| Fleet Pings | Pings: channel, target, message | partial: add fleet type, formup, comms, doctrine |
| Secure Groups (Smart Groups, filters, grace) | the Compliance Group is one fixed filter | after v1 |
| AA SRP, AFAT | none | not planned: NMU doesn't use them |
| AA-Discordbot | none | skip: Tether's Discord is REST only |
| Blacklist ("Pilot Log", Blacklist state) | a high-priority state can already act as a blacklist | notes: after v1 |
| Corp Tools, CorpStats 2.0 | overlap with Member Audit and Corporation Stats | skip |

## Deliberately different

- No email, no telemetry, no CDNs; purpose-built admin pages instead of the Django admin.
- Permissions go to states and groups only, never to single users (F6).
- Compliance, Corporation Stats and the Compliance Group are core, not apps.
- Plugins never see tokens; data access is checked and logged per call.
- Discord only for v1; other services on request, as plugins.

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

After v1: Menu customization, Secure Groups (Smart Groups with filters and grace), Blacklist notes.

## Decisions (2026-09-25)

1. **Apps, not plugins, in the UI.** Admins and users see "Apps" (Admin → Apps), as in AA. The SDK, WIT, manifest and code keep "plugin", the accurate technical term.
2. **Order.** Tasks 1 to 5 above come first, then the Moon Mining and Member Audit plugins, then tasks 6 to 12, then the rest of milestone 2. They are in `docs/PRD.md`.
3. **Other AA apps.** NMU uses none beyond Member Audit and Moon Mining for now, so Fleet Activity Tracking, Structure Timers, Structures, SRP and HR Applications stay off the plan until asked.
