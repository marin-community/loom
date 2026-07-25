-- Agent-backed watches are explicit and paced. Existing status watches need
-- the `judge` capability to keep using the one-shot agent after that primitive
-- becomes capability-gated.
UPDATE watches
   SET capabilities = json_insert(capabilities, '$[#]', 'judge'),
       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
 WHERE program = 'builtin:status'
   AND json_valid(capabilities)
   AND NOT EXISTS (
       SELECT 1 FROM json_each(capabilities) WHERE value = 'judge'
   );

-- Preserve user-customized triggers, but move the untouched stock status
-- watcher off every finished turn and onto the one-shot stale edge.
UPDATE watches
   SET trigger_spec = '{"on":["session.stale"]}',
       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
 WHERE name = 'status'
   AND program = 'builtin:status'
   AND trigger_spec = '{"on":["session.idle"]}';

-- Agentic judgement is opt-in. The two advisory-only PR programs are also
-- opt-in until they perform a real action rather than only recording `would`.
UPDATE watches
   SET enabled = 0,
       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
 WHERE (name = 'status' AND program = 'builtin:status')
    OR (name = 'pr-label' AND program = 'builtin:pr-label')
    OR (name = 'archive-merged' AND program = 'builtin:archive-merged');
