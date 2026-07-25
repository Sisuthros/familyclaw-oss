# Scenario — double refund after crash

1. Agent receives "refund order ORD-100 €25".
2. Side effect fires (PSP charge reverse).
3. Process is killed before the durable commit lands.
4. On resume, a naive agent re-fires; FamilyClaw must report overcount 0.
