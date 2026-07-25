# Scenario — double teardown

Idle stack marked for delete. Crash after the cloud API accepts delete but
before journal commit. Resume must not issue a second delete for the same key.
