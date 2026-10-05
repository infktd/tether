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
