-- Retire the plan subsystem's last schema footprint: the `issues.plan_task`
-- link key (`"<slug>#T3"`) that joined a plan file's tasks to the issue ledger.
-- Plans are no longer a weaver noun — a plan is just an artifact named `plan`
-- whose task list *references* issues (`- #41 …`), so there is nothing to link
-- back to. See docs/artifacts.md.
ALTER TABLE issues DROP COLUMN plan_task;
