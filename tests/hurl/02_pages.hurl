# Pages, assets and security headers. Run against a fresh stack (no owner).

GET {{base}}/
HTTP 303
[Asserts]
header "Location" == "/setup"

GET {{base}}/setup
HTTP 200
[Asserts]
header "Content-Type" contains "text/html"
header "Content-Security-Policy" contains "script-src 'self'"
header "Content-Security-Policy" contains "img-src 'self' data: https://images.evetech.net"
header "Content-Security-Policy" contains "form-action 'self' https://discord.com;"
header "X-Frame-Options" == "DENY"
header "X-Content-Type-Options" == "nosniff"
header "Strict-Transport-Security" exists
body contains "Enter the setup token"
body contains "action=\"/setup/unlock\""

GET {{base}}/login
HTTP 200
[Asserts]
body contains "Log in with EVE Online"
body contains "href=\"/auth/login\""

# Signed out: the Dashboard sends you to log in.
GET {{base}}/dashboard
HTTP 303
[Asserts]
header "Location" == "/login"

# The old address moved to AA's name.
GET {{base}}/profile
HTTP 308
[Asserts]
header "Location" == "/dashboard"

POST {{base}}/profile/main
Origin: {{base}}
[Form]
character_id: 1
HTTP 401

# The wizard's form endpoints refuse without a setup session.
POST {{base}}/setup/sso
Origin: {{base}}
[Form]
client_id: 0123456789abcdef0123
HTTP 401

POST {{base}}/setup/check
Origin: {{base}}
HTTP 401

POST {{base}}/setup/alliance/search
Origin: {{base}}
[Form]
name: Goonswarm Federation
HTTP 401

POST {{base}}/setup/alliance
Origin: {{base}}
[Form]
entity_id: 1
HTTP 401

GET {{base}}/static/app.css
HTTP 200
[Asserts]
header "Content-Type" == "text/css; charset=utf-8"
header "ETag" exists
body contains "--accent-surface"

GET {{base}}/static/htmx.min.js
HTTP 200
[Asserts]
header "Content-Type" == "text/javascript; charset=utf-8"

GET {{base}}/static/fonts/Geist-Variable.woff2
HTTP 200
[Asserts]
header "Content-Type" == "font/woff2"
header "Cache-Control" contains "immutable"

GET {{base}}/static/fonts/GeistMono-Variable.woff2
HTTP 200

GET {{base}}/no-such-page
HTTP 404
[Asserts]
header "Content-Type" contains "text/html"
body contains "nothing at this address"

# Dev-only features must not exist in a release build.
GET {{base}}/dev/login
HTTP 404

GET {{base}}/docs
HTTP 404

# Admin pages: signed out, every one sends you to log in.
GET {{base}}/admin
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/admin/autogroups
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/autogroups
Origin: {{base}}
[Form]
states: 1
corp_groups: on
corp_prefix: Corp
corp_source: name
alliance_prefix: Alliance
alliance_source: name
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/autogroups/1
Origin: {{base}}
[Form]
states: 1
corp_groups: on
corp_prefix: Corp
corp_source: name
alliance_prefix: Alliance
alliance_source: name
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/autogroups/1/delete
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/admin/groups
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/admin/groups/1
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/tokens
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/tokens/1/refresh
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/tokens/1/delete
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/notifications
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/notifications/1
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/notifications/1/delete
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/notifications/read-all
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/notifications/delete-read
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/static/notifications.js
HTTP 200
[Asserts]
header "Content-Type" contains "javascript"

GET {{base}}/groups
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/groups/1
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/group-management
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/group-management/membership
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/group-management/1
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/group-management/1/audit
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/groups/1/join
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/groups/1/leave
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/groups/1/retract
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/group-management/1/requests/1/accept
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/group-management/1/requests/1/reject
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/group-management/1/members/1/remove
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/groups/settings
Origin: {{base}}
[Form]
auto_leave: on
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/groups/reserved
Origin: {{base}}
[Form]
name: directors
reason: CEO only
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/groups/reserved/remove
Origin: {{base}}
[Form]
name: directors
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/groups/1/settings
Origin: {{base}}
[Form]
description: Officers
internal: on
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/groups/1/leaders
Origin: {{base}}
[Form]
character: Chribba
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/groups/1/leaders/1/remove
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/groups/1/leader-groups
Origin: {{base}}
[Form]
group_id: 2
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/groups/1/leader-groups/2/remove
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/admin/permissions
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/admin/states
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/groups
Origin: {{base}}
[Form]
name: Officers
internal: on
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/permissions/grant
Origin: {{base}}
[Form]
permission: admin.audit
grantee: state:3
HTTP 303

POST {{base}}/admin/states
Origin: {{base}}
[Form]
name: Trial
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/states/1/covers
Origin: {{base}}
[Form]
entity_id: 1
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/states/1/covers/1/remove
Origin: {{base}}
[Form]
confirm: 1
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/states/1/move
Origin: {{base}}
[Form]
direction: up
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/states/4/priority
Origin: {{base}}
[Form]
priority: 10
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/states/4/rename
Origin: {{base}}
[Form]
name: Other
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/states/4/delete
Origin: {{base}}
[Form]
confirm: 1
HTTP 303

POST {{base}}/admin/states/search
Origin: {{base}}
[Form]
state_id: 1
name: Goonswarm Federation
HTTP 401

# Discord: the admin page and linking need a session.
GET {{base}}/admin/discord
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/discord
Origin: {{base}}
[Form]
application_id: 1
guild_id: 2
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/discord/names
Origin: {{base}}
[Form]
state_id: 1
format: [{corp_ticker}] {character_name}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/discord/options
Origin: {{base}}
[Form]
sync_names: on
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/services
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/discord/mappings
Origin: {{base}}
[Form]
role_id: 1
grantee: state:1
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/discord/mappings/1/remove
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/services/discord/link
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/services/discord/unlink
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

# A callback that no browser started.
GET {{base}}/discord/callback?code=x&state=y
HTTP 400
[Asserts]
header "Content-Type" contains "text/html"
body contains "expired or was already used"

# Fleet pings need a session (and fleet.ping).
GET {{base}}/pings
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/pings
Origin: {{base}}
[Form]
channel_id: 1
target: everyone
message: Form up
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/discord/channels
Origin: {{base}}
[Form]
channel_id: 1
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/discord/channels/1/remove
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

# The admin dashboard and audit log need a session.
GET {{base}}/admin/system
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/admin/audit
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/system/updates
Origin: {{base}}
[Form]
enabled: on
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/system/updates/check
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/jobs/1/retry
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

# Plugins: every page and action needs a session (and admin.plugins).
GET {{base}}/admin/plugins
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/admin/plugin-uploads/1
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/admin/plugins/nmu.hello
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/admin/plugin-keys/nmu.hello
HTTP 303
[Asserts]
header "Location" == "/login"

# Refused before the body is read.
POST {{base}}/admin/plugins
Origin: {{base}}
[MultipartFormData]
package: not a package
signature: not a signature
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/plugin-uploads/1/approve
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/plugin-uploads/1/discard
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/plugins/nmu.hello/enable
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/plugins/nmu.hello/disable
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/plugins/nmu.hello/uninstall
Origin: {{base}}
[Form]
confirmation: nmu.hello
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/plugin-keys/nmu.hello
Origin: {{base}}
[Form]
expected_old: RWQ
new_key: RWQ
confirmation: nmu.hello
HTTP 303
[Asserts]
header "Location" == "/login"

# Plugin pages need a session (then their permission).
GET {{base}}/plugins/nmu.hello
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/plugins/nmu.hello/some/page
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/plugins/nmu.hello
Origin: {{base}}
[Form]
_form: note
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/plugins/nmu.hello/some/page
Origin: {{base}}
[Form]
_form: note
HTTP 303
[Asserts]
header "Location" == "/login"

# Registering characters, Corp Stats and data sources need a session
# (approving: admin.states or admin.plugins).
GET {{base}}/register
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/register/start
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/profile/corp-stats/offer
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/profile/corp-stats/1/withdraw
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/compliance
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/compliance/sources/1/approve
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/compliance/sources/1/remove
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/states/1/scopes
Origin: {{base}}
[Form]
scope: esi-skills.read_skills.v1
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/states/1/scopes/remove
Origin: {{base}}
[Form]
scope: esi-skills.read_skills.v1
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/profile/plugins/nmu.hello/offer
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/profile/plugins/nmu.hello/offer/1/withdraw
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/plugins/nmu.hello/sources/1/approve
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/plugins/nmu.hello/sources/1/remove
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/plugins/nmu.hello/channels
Origin: {{base}}
[Form]
channel_id: 1
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/plugins/nmu.hello/channels/1/remove
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"
