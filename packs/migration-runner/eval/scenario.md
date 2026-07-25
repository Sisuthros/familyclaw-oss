# Scenario — re-applied migration step

Steps 0..3 alter schema. Crash inside step 1 after the DDL applied. Naive
resume re-runs step 1 and corrupts data. FamilyClaw must keep overcount 0.
