-- The stock PR labeller now performs the label write it previously only
-- advised. Activate unchanged stock rows parked by migration 0014.
UPDATE watches
   SET enabled = 1,
       capabilities = json_insert(capabilities, '$[#]', 'mark'),
       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
 WHERE name = 'pr-label'
   AND program = 'builtin:pr-label'
   AND trigger_spec = '{"on":["pr.opened"]}'
   AND scope = '{}'
   AND params = '{"label":"weaver"}'
   AND json_valid(capabilities)
   AND NOT EXISTS (
       SELECT 1 FROM json_each(capabilities) WHERE value = 'mark'
   );
