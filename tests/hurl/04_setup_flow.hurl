# The first-run wizard's API against a fresh stack. Needs
# --variable setup_token=<SETUP_TOKEN from deploy/.env>.

GET {{base}}/api/setup
HTTP 200
[Asserts]
jsonpath "$.state" == "needs_sso"
jsonpath "$.unlocked" == false
jsonpath "$.callback_url" == "{{base}}/auth/callback"

POST {{base}}/api/setup/unlock
Origin: {{base}}
{"token": "not-the-token"}
HTTP 403

POST {{base}}/api/setup/unlock
Origin: {{base}}
{"token": "{{setup_token}}"}
HTTP 204
[Asserts]
header "Set-Cookie" contains "__Host-tether_setup="
header "Set-Cookie" contains "HttpOnly"
header "Set-Cookie" contains "Secure"

GET {{base}}/api/setup
HTTP 200
[Asserts]
jsonpath "$.unlocked" == true

POST {{base}}/api/setup/sso
Origin: {{base}}
{"client_id": "0123456789abcdef0123456789abcdef"}
HTTP 200
[Asserts]
jsonpath "$.callback_url" == "{{base}}/auth/callback"

GET {{base}}/api/setup/probe?nonce=hurl123
HTTP 200
[Asserts]
body == "tether-probe:hurl123"

# From inside the app container, the public URL may or may not be
# reachable (DOMAIN=localhost isn't); either way the check answers.
POST {{base}}/api/setup/callback-check
Origin: {{base}}
HTTP 200
[Asserts]
jsonpath "$.ok" isBoolean
jsonpath "$.detail" isString

GET {{base}}/api/setup
HTTP 200
[Asserts]
jsonpath "$.state" == "needs_owner"

# SSO is configured, so login now redirects to CCP with PKCE.
GET {{base}}/auth/login
HTTP 303
[Asserts]
header "Location" startsWith "https://login.eveonline.com/v2/oauth/authorize"
header "Location" contains "code_challenge_method=S256"
header "Set-Cookie" contains "__Host-tether_login="

# The same wizard through its pages: the unlocked browser sees the owner step.
GET {{base}}/setup
HTTP 200
[Asserts]
body contains "Log in as the owner"
