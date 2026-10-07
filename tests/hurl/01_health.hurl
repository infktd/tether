# Liveness, readiness and the API spec.

GET {{base}}/health
HTTP 200
[Asserts]
body == "ok"

GET {{base}}/ready
HTTP 200
[Asserts]
body == "ready"

# The API spec, like every route but the way in, needs a login.
GET {{base}}/api/openapi.json
HTTP 401

# Metrics are off unless METRICS_ENABLED: the same 404 as an address that
# doesn't exist, token or not, and never indexed.
GET {{base}}/metrics
HTTP 404
[Asserts]
header "X-Robots-Tag" contains "noindex"
body contains "nothing at this address"

GET {{base}}/metrics
Authorization: Bearer 0123456789abcdef0123456789abcdef0123456789abcdef
HTTP 404
