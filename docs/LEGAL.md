# Licenses and CCP's terms

This answers two of the PRD's open questions: the licenses of the Alliance Auth apps Tether matches, and what CCP's Developer License Agreement asks of an instance. It was checked on 2026-09-27. This is an engineer's reading, not legal advice.

## The Alliance Auth apps Tether matches

Tether is GPL-2.0-or-later. It reimplements behaviour; it doesn't port code.

| Project | License | In Tether |
| --- | --- | --- |
| Alliance Auth (core: groups, states, services, HR applications, timerboard, optimer, SRP, fleet activity tracking, corporation stats) | GPL-2.0 | core, and the bundled apps named after it |
| aa-memberaudit | MIT | Member Audit |
| aa-moonmining | MIT | Moon Mining |
| aa-structures | MIT | Structures |
| aa-structuretimers | MIT | Structure Timers' extras |
| allianceauth-secure-groups | MIT | Secure Groups |
| allianceauth-blacklist | MIT | Blacklist and Pilot Log |
| aa-fleetpings | GPL-3.0 | Fleet Pings |
| allianceauth-afat | GPL-3.0 | Fleet Activity Tracking's extras |
| aa-srp | GPL-3.0 | Ship Replacement's extras |
| allianceauth-fittings | GPL-3.0 | Fittings |

### What was checked

Every string literal and template text of 35 characters or more in each project was compared with Tether's code and templates (docs excluded), whitespace-normalised. The scan excluded tests, migrations and translations.

The only matches were:

- **Permission descriptions:** AA's own words, kept on purpose so admins recognise what they grant. Six are from MIT apps, and one ("Can see statistics of other corporations") is from allianceauth-afat.
- **Notification type names and one sentence:** from aa-structures (MIT). The sentence is "Within 24 hours fighting can legally occur between those involved", which is CCP's own game text.
- **Three short sign-in and Discord messages:** from Alliance Auth core (GPL-2.0, the same license family as Tether's).

No code, and nothing beyond short labels, came from the GPL-3.0 apps. So Tether stays GPL-2.0-or-later, and the rule stays: match their behaviour, never copy their code.

The MIT apps whose text appears have their copyright and permission notice in `NOTICE.md`, as MIT asks.

## CCP's Developer License Agreement

These are the clauses that bear on an instance. The agreement is at developers.eveonline.com/license-agreement, and it shows no date.

- **7.1, proprietary notice:** the notice must be kept. Tether shows it, word for word, on the sign-in page and at the foot of every page.
- **7.3, marks:** CCP's logos can't be combined with other marks. Tether's wordmark and glyph are its own. The EVE logo isn't used, and CCP's images (portraits, logos, type icons) come from CCP's image server unaltered.
- **4.1 and 4.4, non-commercial:** the rights are for non-commercial use. Tether never charges. An instance's admins may take ISK or donations toward hosting (4.4), but not money for access.
- **2.3(c), consent:** no tracking of a player's information without their express knowledge and consent.
  - Tether reads a character only with the scopes its owner granted through EVE SSO: registering for the host or an app, or adding it as an app's data source (a Station Manager or Director agreeing to it).
  - Apps never see tokens.
  - An app sees only what the admin approved at install, and pilots see which apps read their characters (Token Management).
- **2.5, rate limits:** ESI calls stay inside ESI's error and rate limits: one shared budget, cached responses, and back-off per owner.
- **9.5, privacy law:** each instance must comply with the privacy and data protection laws that apply to it. Its admins run it, so they are its data controllers. Tether helps them with this:
  - it keeps only what the features need;
  - Member Audit deletes what's past its retention setting;
  - a deleted token's character leaves the account;
  - backups and snapshots stay on the instance.
- **Retention:** the agreement sets no limit on how long ESI data may be kept, and says nothing about open-sourcing a tool.

### What admins should know

An instance's admins should tell their members what Tether stores and who can see it, as any tool holding character data should (GDPR, where it applies). A short notice in the alliance's usual place is enough. Tether leaves the wording to them.
