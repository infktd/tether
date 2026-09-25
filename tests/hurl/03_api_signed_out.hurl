# Every signed-in and admin endpoint refuses without a session, and
# state-changing requests need a same-origin Origin.

GET {{base}}/api/me
HTTP 401

POST {{base}}/api/me/main
Origin: {{base}}
{"character_id": 1}
HTTP 401

GET {{base}}/api/notifications
HTTP 401

GET {{base}}/api/notifications/unread
HTTP 401

POST {{base}}/api/notifications/1/open
Origin: {{base}}
HTTP 401

DELETE {{base}}/api/notifications/1
Origin: {{base}}
HTTP 401

POST {{base}}/api/notifications/read-all
Origin: {{base}}
HTTP 401

POST {{base}}/api/notifications/delete-read
Origin: {{base}}
HTTP 401

GET {{base}}/notifications/stream
HTTP 401

GET {{base}}/api/groups
HTTP 401

POST {{base}}/api/groups/1/join
Origin: {{base}}
HTTP 401

POST {{base}}/api/groups/1/leave
Origin: {{base}}
HTTP 401

POST {{base}}/api/groups/1/retract
Origin: {{base}}
HTTP 401

GET {{base}}/api/group-management/requests
HTTP 401

POST {{base}}/api/group-management/groups/1/requests/1/accept
Origin: {{base}}
HTTP 401

POST {{base}}/api/group-management/groups/1/requests/1/reject
Origin: {{base}}
HTTP 401

GET {{base}}/api/group-management/groups/1/members
HTTP 401

DELETE {{base}}/api/group-management/groups/1/members/1
Origin: {{base}}
HTTP 401

GET {{base}}/api/group-management/groups/1/audit-log
HTTP 401

POST {{base}}/api/admin/groups
Origin: {{base}}
{"name": "Officers"}
HTTP 401

PUT {{base}}/api/admin/groups/1
Origin: {{base}}
{"internal": true, "hidden": true, "open": false, "public": false, "restricted": false}
HTTP 401

DELETE {{base}}/api/admin/groups/1
Origin: {{base}}
HTTP 401

POST {{base}}/api/admin/groups/1/members
Origin: {{base}}
{"account_id": 1}
HTTP 401

DELETE {{base}}/api/admin/groups/1/members/1
Origin: {{base}}
HTTP 401

PUT {{base}}/api/admin/groups/1/leaders/1
Origin: {{base}}
HTTP 401

DELETE {{base}}/api/admin/groups/1/leaders/1
Origin: {{base}}
HTTP 401

PUT {{base}}/api/admin/groups/1/leader-groups/2
Origin: {{base}}
HTTP 401

DELETE {{base}}/api/admin/groups/1/leader-groups/2
Origin: {{base}}
HTTP 401

GET {{base}}/api/admin/reserved-group-names
HTTP 401

POST {{base}}/api/admin/reserved-group-names
Origin: {{base}}
{"name": "directors", "reason": "CEO only"}
HTTP 401

DELETE {{base}}/api/admin/reserved-group-names/directors
Origin: {{base}}
HTTP 401

GET {{base}}/api/admin/permissions
HTTP 401

POST {{base}}/api/admin/permissions/grants
Origin: {{base}}
{"permission": "admin.audit", "state_id": 1}
HTTP 401

DELETE {{base}}/api/admin/permissions/grants/1
Origin: {{base}}
HTTP 401

GET {{base}}/api/admin/audit
HTTP 401

POST {{base}}/api/admin/accounts/1/deactivate
Origin: {{base}}
HTTP 401

POST {{base}}/api/admin/accounts/1/reactivate
Origin: {{base}}
HTTP 401

GET {{base}}/api/admin/states
HTTP 401

POST {{base}}/api/admin/states
Origin: {{base}}
{"name": "Trial"}
HTTP 401

PATCH {{base}}/api/admin/states/4
Origin: {{base}}
{"name": "Other"}
HTTP 401

DELETE {{base}}/api/admin/states/4
Origin: {{base}}
HTTP 401

POST {{base}}/api/admin/states/1/move
Origin: {{base}}
{"direction": "up"}
HTTP 401

POST {{base}}/api/admin/states/1/covers
Origin: {{base}}
{"entity_id": 1}
HTTP 401

DELETE {{base}}/api/admin/states/1/covers/1
Origin: {{base}}
HTTP 401

POST {{base}}/api/admin/states/1/scopes
Origin: {{base}}
{"scope": "esi-skills.read_skills.v1"}
HTTP 401

DELETE {{base}}/api/admin/states/1/scopes/esi-skills.read_skills.v1
Origin: {{base}}
HTTP 401

POST {{base}}/api/admin/states/resolve
Origin: {{base}}
{"names": ["Goonswarm Federation"]}
HTTP 401

# CSRF: no Origin (and no Sec-Fetch-Site) or a foreign one is refused
# before anything else runs.
POST {{base}}/auth/logout
HTTP 403

POST {{base}}/api/setup/unlock
Origin: https://evil.example
{"token": "x"}
HTTP 403
[Asserts]
body contains "Cross-site request blocked"

# The SSO endpoints before setup: login isn't configured yet, and a bare
# callback is rejected.
GET {{base}}/auth/login
HTTP 503

GET {{base}}/auth/callback
HTTP 400
