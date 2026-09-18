// One reader for the server's presence evidence.
//
// The sensing server publishes one presence verdict per frame inside
// `calibrated_presence_evidence`: the verdict itself, the identity of the
// calibration it was computed against (`model_id`, `session_id`,
// `binding_digest`), and the basis of the verdict (`presence_authority`,
// `duty_share`, `duty_window_ms`).
//
// `presence_authority` decides how to read `person_count`. A verdict that
// motion evidence asserted claims no count, so `person_count: 0` means "not
// claimed", never "empty room" — read `presence` for the verdict.
//
// Frames from a server without an explicit calibration still carry the same
// verdict in `classification.presence` beside `estimated_persons`, so this one
// reader falls back to those instead of every page deriving its own answer.
export function presenceEvidenceOf(message) {
  const evidence = message && message.calibrated_presence_evidence;
  if (evidence && typeof evidence.presence === 'boolean') {
    return {
      presence: evidence.presence,
      persons: typeof evidence.person_count === 'number' ? evidence.person_count : 0,
      authority: typeof evidence.presence_authority === 'string'
        ? evidence.presence_authority
        : 'unknown',
      method: typeof evidence.inference_method === 'string' ? evidence.inference_method : null,
      modelId: typeof evidence.model_id === 'string' ? evidence.model_id : null,
      dutyShare: typeof evidence.duty_share === 'number' ? evidence.duty_share : null,
      dutyWindowMs: typeof evidence.duty_window_ms === 'number' ? evidence.duty_window_ms : null,
      calibrated: true,
    };
  }

  const classification = (message && message.classification) || {};
  return {
    presence: typeof classification.presence === 'boolean' ? classification.presence : null,
    persons: message && typeof message.estimated_persons === 'number'
      ? message.estimated_persons
      : 0,
    authority: 'classification',
    method: null,
    modelId: null,
    dutyShare: null,
    dutyWindowMs: null,
    calibrated: false,
  };
}

/** Human-readable label for a verdict's authority, for the presence readout. */
export function presenceAuthorityLabel(authority) {
  switch (authority) {
    case 'calibrated_occupancy': return 'calibrated room model';
    case 'motion_evidence': return 'motion evidence (no count)';
    case 'classification': return 'room classification';
    case 'none': return 'no evidence';
    default: return authority || 'unknown';
  }
}
