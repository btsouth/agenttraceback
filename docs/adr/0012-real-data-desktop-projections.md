# ADR 0012: Real-Data Desktop Projections

The desktop renders API projections from the daemon store. There is no fixture-data
fallback in production paths. Demo mode inserts an explicitly labeled project into
the same store and can remove it through the audited deletion path.

`VERIFIED` remains a projection: persisted source events stay `REPORTED` or
`OBSERVED`, and timeline/query projections mark an observed event verified only when
an active correlation row exists. Raw content is never rendered as HTML.
