# Changelog

What changed in each Tether update, newest first. Tether shows the entries a pilot hasn't seen in a "What's new" popup after an update: the **Everyone** notes to every pilot, an app's notes to whoever may open that app, and the **Admins** notes to whoever may open Administration. Every update that changes what Tether does adds an entry at the top (`scripts/check-changelog.sh`).

Each entry is a `## YYYY-MM-DD` heading (EVE time), then `### Everyone`, `### Admins` or `### <app name>` headings, each with `- ` notes in plain words. Never remove or reorder entries: pilots' "seen" marks count them from the bottom.

## 2026-10-07

### Contracts
- Buyback contracts, the ones with a Buyback tracking number in their description, are left to the Buyback app, so each gets one Discord card instead of two.

### Buyback
- Leaderboards open to everyone who can use the program, and performance to holders of "see performance" and program managers, as in Alliance Auth.

## 2026-10-07

### Member Audit
- Skill sets now have a required level, a recommended level or both for each skill, plus a description, a ship and an option to hide them from pilots' sheets.
- Skill set groups, doctrines among them, sort the sets on the sheet and in Reports.
- Skill sets can be changed and copied on their own page, can name any skill, and there's no limit of 30.
- Make a skill set from a fitting: paste it in EFT format and it requires every skill the ship and its items need.
- The Skill Sets report shows each character's main, state, organisation, whether it's their main, and whether the group is a doctrine, with filters.
- New reports: User compliance and Corporation compliance.
- The Character finder lists every character of the pilots you can see, including ones not registered, with the main marked and more filters.
- A character's sheet shows whose character it is and their other characters.
- Contracts are kept until they've been expired for the keep period, and the mining ledger is no longer cut at 90 days.

### Moon Mining
- An extraction stays on Upcoming until 12 hours after it pops, then moves to Past, so a moon that just popped no longer vanishes from both tabs.
- Refineries your corporation no longer has stop counting as owning their moons.
- New setting: price ores by what they refine into, at a reprocessing yield you choose (85% by default).
- Moons can have labels, and the Moons list filters by owner, region, ore type and label.

### Structures
- Structures out of fuel show Low power, Abandoned (after 7 days with no service online) or Abandoned? (never seen online).
- Every tab says when it shows only part of a long list, and how many there are.
- Fuel and jump fuel alerts can be edited, disabled and enabled again.
- Send a test notification to a channel from Settings, and hear in your notifications whether it arrived.

### Ship Replacement
- You're told when your SRP request is approved or rejected, with the reviewer's comment.
- Open your request from My SRP requests to see where it stands, why it was rejected and its history.
- SRP managers can remove a single request; its loss can then be requested again.
- SRP managers can edit a fleet's after action report.
- SRP managers can disable a fleet to stop new requests for now, and enable it again.

### HR Applications
- You're told when your application is taken for review, approved, rejected or deleted.
- Review's search also finds applicants by their characters' corporations and alliances.

### Blueprints
- Copies can be requested only of originals, and never of reaction formulas.
- When a pilot cancels their request, the builders hear about it on Discord, and the builder who took it also gets a notification.

### Freight
- My Alliance now keeps only contracts from the alliance's own members, as in Alliance Auth.
- Customers hear about contracts on priced routes only, unless "Announce every contract" is on.
- My contracts now needs "Can use the calculator", as in Alliance Auth, and shows outstanding, in-progress, finished and failed contracts.
- Contracts has an All tab beside Active, with every contract.
- Discord cards open Tether: new contracts open Contracts, customer cards open My contracts.

### Contacts
- A contact removed in EVE stays while it has notes or server links, at standing 0, marked "Not in EVE's list".
- Server links can be edited on their own page, in any of Alliance Auth's eight colours.
- Every contact is listed, 500 a page, and the search finds any of them by name or label.

### Contracts
- Discord cards open the Contracts page in Tether.

### Fleet Activity Tracking
- A link that tracks your ESI fleet no longer expires: it stays open until the fleet ends, and closes when tracking stops.
- Paste the fleet composition from EVE's fleet window on a link's Fleet snapshot tab, and everyone in it gets a FAT with ship and system.
- A passing ESI error no longer ends tracking: it stops only when the same error comes back several times in a row, and the link's page shows how many.

### Sovereignty Timer
- Progress shows the score before its last change beside the score now, and the trend no longer resets after a quiet minute.

### ESI Status
- See how many routes have each status, and their share of all routes.

### Time Zones
- An adjusted time counts down to itself, and says "Already over" once it has passed.

### Bulletin Board
- Pick who may read a bulletin as you write it.

### Admins
- Apps can now send a notice to someone who submitted one of their forms, such as an applicant or a requester, even if that person holds none of the app's permissions. Nobody else is reachable this way.
- Member Audit can now see your members' characters that aren't registered, and their mains, for its Character finder. Only the included Member Audit can; other apps never do.
- Fleet Activity Tracking: `manage_afat` now opens every corporation's and alliance's statistics, as in Alliance Auth. Check who holds it.
- Freight: My contracts now needs "Can use the calculator". Give it to customers who should see their contracts.
- Approve the updated apps on the Apps page to get these changes.

## 2026-10-07

### Everyone
- Press ⌘K (Ctrl K on Windows and Linux), or use the search in the top bar, to jump to any page or action you can use.
- Under your account menu, Sessions lists the browsers you're signed in on, so you can sign out of one or of all the others.
- Lists across Tether have a search and filters that stay in the address, so a link or a refresh keeps what you were looking at.
- Better on phones: filters fold into one button, a page's main button sits at the bottom of the screen, and there's a "Skip to content" link and clearer keyboard focus.

### Buyback
- New app: sell items to your corporation's buyback programs. Paste your items from the game's inventory, get a price and a tracking number, and contract them to the program's manager. Tether checks the contract against your calculation and tells you when it's accepted or rejected.
- Reverse buyback: buy from your corporation's hangar stock, with your items set aside until your contract comes in.
- Statistics for your own contracts, leaderboards by month and, for managers, their programs' contracts and wallets.

### Admins
- Buyback is a new included app (Alliance Auth's aa-buybackprogram): approve it on the Apps page, then add a manager as its data source, add locations and create programs. Prices come from Fuzzwork, or from Janice with its API key.
- The audit log can be filtered by who, action, app and date, and exported as a CSV.
- Admins with user management can sign a user out everywhere from the user's page.
- An optional Prometheus metrics endpoint, off unless METRICS_ENABLED and METRICS_TOKEN are set (deploy/README.md).
- Every release on GitHub now has its notes from this changelog, and there are guides for admins, members and app developers in the repository.

## 2026-10-07

### Structures
- Corporations with very large asset lists now show their structures' fittings, quantum cores, fuel and moon material bays, and their Orbital Skyhooks: Tether reads every page of the assets in the background, as Alliance Auth does.
- A problem with one of an owner's reads stays shown until that read works again.

### Ship Replacement
- The SRP team's card in Discord links to the fleet's requests in Tether, as in Alliance Auth.

## 2026-10-07

### Everyone
- A "What's new" popup like this one now opens once after each update. Every update's notes stay under your account menu, What's new.

### Member Audit
- Every page of a character's assets is read now, however large the hangar, as in Alliance Auth. The full wallet journal and older mail are read too, a few pages each update.
- A character whose login stopped working keeps what was read, with its updates paused, until you log it in again (Token Management or Register Character).
- Mail deleted in EVE disappears from the character sheet.
- Fixed: large alliances, characters sharing a planet, and passing ESI errors no longer stop updates or blank mail and contract items.

### Structures
- A Jump gates tab lists the Ansiblex gates you can see, with their liquid ozone.

### Structure Timers
- Moon extractions from Structures now show up here as timers, as in Alliance Auth.

### Moon Mining
- Extractions and refineries refresh every 10 minutes and mining ledgers every hour, as in Alliance Auth. Every corporation's are read, not only the first few.

### ESI Status
- ESI is checked every minute, as in Alliance Auth.

### Ship Replacement
- Fleets with more than 400 requests show all of them.

### Freight
- Pilot notices can mention a Discord role, as in Alliance Auth.

### Blueprints
- Blueprints in corporation hangars and containers get their locations, however many assets the corporation has.

### Admins
- New instances start with nobody allowed to join Discord, as in Alliance Auth; this instance keeps its grants. The Discord page says when a role is mapped to a state that can't link.
- Pages now say when a setting can't take effect, with the fix: apps open to every pilot, Fleet Pings nobody may send, data sources nobody may add, groups nobody may ask for.
- Permissions are listed in plain words by area, each with who it's for.
- Member Audit can tell pilots when it can't read one of their characters. It's off on this instance: turn it on under Administration › Settings › Notifications.
- Structures, Moon Mining and Ship Replacement ask for new abilities (notices to admins, Discord): approve their updates on the Apps page. Until then they keep running their last version.
- Structures starts new installs with Alliance Auth's notification types and pings; Moon Mining, Blueprints and Structures can notify admins; Ship Replacement can post new requests to an SRP team channel.
- Fixed: a Discord channel the bot can't post in no longer blocks every other message, and Secure Groups no longer pile up dead jobs when their update channel is gone.
