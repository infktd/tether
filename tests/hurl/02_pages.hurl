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
