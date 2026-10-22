-- OAuth clients a deployment stack declares (`deployment.reconcile`). Each
-- reconcile updates them in place, and a pruning reconcile removes the ones
-- the stack no longer declares. Clients registered by hand stay 0.
ALTER TABLE oauth_clients ADD COLUMN managed_by_deployment INTEGER NOT NULL DEFAULT 0;
