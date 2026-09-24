# Liveness, readiness and the API spec.

GET {{base}}/health
HTTP 200
[Asserts]
body == "ok"

GET {{base}}/ready
HTTP 200
[Asserts]
body == "ready"

GET {{base}}/api/openapi.json
HTTP 200
[Asserts]
header "Content-Type" contains "application/json"
jsonpath "$.info.title" == "Tether API"
jsonpath "$.components.securitySchemes.session.in" == "cookie"
