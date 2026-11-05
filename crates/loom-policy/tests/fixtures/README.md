`shared_grants.json` is a byte-for-byte copy of Marin's
`lib/rigging/src/rigging/grant_vectors.json`. Its SHA-256 is
`c6e9a02ec01eb884dfd7d79a4f9e2a84e3be3352a3a77a17ece22038d88b7450`.

Both repositories verify these signed tokens with their real JWT libraries and
check the same actor, audience, action and resource contract. The fixture holds
only a synthetic public key; its signing key is discarded. Valid tokens expire
in 2100 so normal test runs do not need a clock override.

The signed `grant_version` must be integer `1`. Both parsers reject missing,
unsupported, boolean, string, float, and null versions.

Update the canonical Marin fixture first, copy it here, update this hash, and
run both conformance tests. These vectors cover token verification; runtime
revocation and session isolation are tested separately against the live policy
endpoint. Finelog's existing cluster-ingress policy is not changed by this
fixture.
