-- A character registered for an app before the app asked for another
-- scope stays registered, and the app reads it with the scopes its token
-- has; its pilot is asked once to register it again. The scopes they were
-- asked for (what the token lacked then), so a scope the app adds later
-- asks again. It goes with the registration.
ALTER TABLE core.app_characters ADD COLUMN scopes_prompted text[];
