-- An app's Data sources page and the notice on its other pages read each
-- source's latest calls (DESIGN.md, App shell): by character, newest first.
CREATE INDEX plugin_access_log_character_idx
    ON core.plugin_access_log (plugin_id, character_id, at DESC);
