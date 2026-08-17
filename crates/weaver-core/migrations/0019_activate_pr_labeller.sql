-- The stock PR labeller now performs its narrowly-scoped action instead of
-- only reporting what it would do. Activate untouched stock rows that migration
-- 0014 deliberately parked while the program was advisory-only, and grant the
-- same `mark` capability new installations receive. Customized copies keep
-- their operator-selected state.
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
