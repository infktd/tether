# Changelog

What changed in each Tether update, newest first. Tether shows the entries a pilot hasn't seen in a "What's new" popup after an update: the **Everyone** notes to every pilot, an app's notes to whoever may open that app, and the **Admins** notes to whoever may open Administration. Every update that changes what Tether does adds an entry at the top (`scripts/check-changelog.sh`).

Each entry is a `## YYYY-MM-DD` heading (EVE time), then `### Everyone`, `### Admins` or `### <app name>` headings, each with `- ` notes in plain words. Never remove or reorder entries: pilots' "seen" marks count them from the bottom.

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
