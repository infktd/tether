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

# Signed out: the profile sends you to log in.
GET {{base}}/profile
HTTP 303
[Asserts]
header "Location" == "/login"

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

GET {{base}}/admin/groups
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/admin/groups/1
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/admin/permissions
HTTP 303
[Asserts]
header "Location" == "/login"

GET {{base}}/admin/tiers
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/groups
Origin: {{base}}
[Form]
name: Officers
join_policy: assigned
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/permissions/grant
Origin: {{base}}
[Form]
permission: admin.audit
grantee: tier:guest
HTTP 303

POST {{base}}/admin/tiers
Origin: {{base}}
[Form]
entity_id: 1
tier: member
HTTP 303

POST {{base}}/admin/tiers/search
Origin: {{base}}
[Form]
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

POST {{base}}/admin/discord/nickname
Origin: {{base}}
[Form]
template: [{corp}] {name}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/discord/mappings
Origin: {{base}}
[Form]
role_id: 1
grantee: tier:member
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/admin/discord/mappings/1/remove
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/profile/discord/link
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/profile/discord/unlink
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

# Plugin ESI access and data sources need a session (admins: admin.plugins).
POST {{base}}/profile/plugins/nmu.hello/consent
Origin: {{base}}
HTTP 303
[Asserts]
header "Location" == "/login"

POST {{base}}/profile/plugins/nmu.hello/consent/1/revoke
Origin: {{base}}
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
