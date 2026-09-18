# ADR 364: Persisted explicit calibration for restart recovery

## Status

Proposed. Implemented on the room B fork branch `fix/grid-population-homogeneity`
and awaiting upstream review; not merged upstream.

## Date

2026-09-18

## Context

An explicit empty-room calibration costs the operator minutes of an empty room.
On the room B rig the accepted hold is three minutes of a room that must stay
still and unoccupied, and the quality gate only accepts it when the residual p95
stays under the limit, which is easiest early in the morning.

That measurement lived only in process memory. A deploy, a crash, or a power
cycle discarded it, and the rig then had no calibrated occupancy until another
hold was taken. MEASURED on this rig: after a restart the room reported absence
for a seated operator until recalibration, because the negative-only bootstrap
prior of ADR-355 can suppress a false count but cannot authorize calibrated
evidence.

ADR-355 deliberately keeps the persisted bootstrap prior at negative-only
authority, and that decision is not reopened here. What is missing is the
operator's own completed hold: it is a real measurement of this room, taken by
the operator, with a full receipt identity (boot epoch, session, model id,
binding digest, frame count, variance explained, completion time).

## Decision

1. After a successful finalize, the server persists the completed field model
   together with the receipt identity of the hold, the frozen CSI grid, and the
   source node set.
2. On startup, the image is restored when schema, authority, installation
   binding, payload digest, file size, and lifetime all verify. Any failure is
   refused whole: no partial restore, no fallback to an unverified model.
3. A restored calibration carries **occupancy authority only**. It may publish
   occupancy and presence evidence bound to its receipt identity. It never
   authorizes numeric vital signs, which stay gated on a calibration completed
   in the running process (`explicit_calibration_fresh_at`).
4. Evidence published against a restored calibration is marked `restored: true`,
   so a consumer can distinguish it from a verdict computed against a hold taken
   in this process.
5. `GET /api/v1/calibration/status` reports
   `binding_mode: restored_calibration`, the full receipt, and the boundary it
   enforces (`occupancy_authorized: true`, `numeric_vitals_authorized: false`).
   A live collection or an active bootstrap prior takes precedence in that
   report, because a live hold is the stronger authority.
6. `POST /api/v1/calibration/reset` removes the persisted image as well as the
   in-memory model, so a reset is not undone by the next restart.
7. The bootstrap prior of ADR-355 keeps its own authority and is only used when
   no explicit image restores. It is never upgraded by the presence of one.
8. The restored calibration binds no live session: `calibration_session_id`
   stays empty, so a restored image can never be mistaken for a collection in
   progress, and stop/reset identity checks behave as before.

## Security and privacy

The image contains aggregate model statistics, configuration, one source node
id and its grid, and the receipt identity. It excludes raw CSI, waveforms,
device addresses, room names, pose, and labels. It is bound to a SHA-256 digest
of the stable installation ID, carries an integrity digest over the exact
stored payload bytes, is capped at 1 MiB, is written 0600 through an atomic
rename, refuses symlinks and non-files, and inherits the field model's expiry.
No new network surface is added; the file is local state in the configured data
directory.

The change reduces no existing gate: numeric vitals, calibrated evidence for a
hold in this process, and bootstrap authority all keep their current rules, and
the restored path is strictly narrower than the hold it restores.

## Consequences

A restart no longer costs an empty hold for presence and occupancy. Numeric
vitals still require a hold taken in the running process, so an operator who
wants heart and breathing numbers after a restart must still hold the room
empty: that asymmetry is deliberate until a restored-vitals authority is argued
on evidence rather than convenience.

Two images may exist at once (an ADR-355 bootstrap prior and this explicit
image). The explicit image wins, and the prior stays on disk, still reported,
still negative-only.

A stale or expired image is refused, and the effect is the pre-ADR behaviour: an
uncalibrated room needs a new hold. A model calibrated for one installation
cannot be restored into another.

## Evidence and acceptance

Software acceptance (SYNTHETIC/unit evidence, executed):

1. `calibration_persistence` round-trips a completed model and its receipt
   identity, and refuses another installation, a tampered payload, an expired
   image, and an oversized file.
2. A restored calibration publishes occupancy evidence with its receipt
   identity and `restored: true`, and `explicit_calibration_fresh_at` stays
   false, so the vitals gate is untouched.
3. A restored calibration does not activate the bootstrap prior of ADR-355.
4. Status reports `binding_mode: restored_calibration` with
   `occupancy_authorized: true` and `numeric_vitals_authorized: false`.

Physical acceptance is **not yet performed**: it requires a completed hold on the
rig, a server restart with the same installation ID and data directory, and a
captured status/stream log showing the restored identity, occupancy evidence,
`restored: true`, and abstained numeric vitals. Until that log exists, the
restart recovery is CLAIMED from unit tests and not MEASURED on hardware.

## Implementation

1. `wifi-densepose-sensing-server::calibration_persistence` owns schema,
   integrity, storage, lifetime, and validation.
2. `calibration_stop` persists the completed model; a failure is logged and
   never fatal, since the in-process calibration is already valid.
3. Startup restores the image before the ADR-355 bootstrap prior and prefers it.
4. `calibrated_presence_evidence` reports `restored` and is gated on
   `occupancy_calibration_active_at`, which is the union of a fresh hold in this
   process and a fresh restored image; numeric vitals keep using
   `explicit_calibration_fresh_at`.
5. `calibration_reset` removes the image; `calibration_status` reports it.


## Amendment: restored vitals authority (same day)

The operator, after using the restored calibration on the rig, judged the
presence-only boundary wrong: the dashboard showed no breathing and no heart
rate, which are the numbers this rig exists to produce, and the reason was the
process restart rather than the quality of the evidence.
`explicit_calibration_fresh_at` no longer excludes a restored calibration, so a
restored image authorizes numeric vitals as well as occupancy.

Guards that remain: an explicit calibration with a complete receipt is still
required, the image still expires with its model, the publication path still
needs exactly one occupant with qualified signal quality and confidence, and
once the ceiling gate is deployed each metric must clear its reporting node's
own measured empty-room ceiling.

CLAIMED, not MEASURED: that a restored calibration's numbers are as good as a
fresh hold's is not yet shown. It needs the seated test repeated after a
restart.
