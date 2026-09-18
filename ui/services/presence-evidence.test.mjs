// Regression tests for the shared presence-evidence reader.
//
// Run: node --test ui/services/presence-evidence.test.mjs
// (also listed in .github/workflows/ci.yml so CI executes it)

import { test } from 'node:test';
import assert from 'node:assert/strict';

import { presenceEvidenceOf, presenceAuthorityLabel } from './presence-evidence.js';

test('the evidence object is the verdict, with its calibration identity', () => {
  const verdict = presenceEvidenceOf({
    calibrated_presence_evidence: {
      schema: 'ruview.calibration.calibrated-presence-evidence.v2',
      model_id: 'cal-model-1',
      inference_method: 'field_model_eigenvalue_v1+duty_cycle_union_v1',
      presence: true,
      person_count: 1,
      presence_authority: 'calibrated_occupancy',
      duty_share: 0.92,
      duty_window_ms: 30000,
    },
    classification: { presence: false, motion_level: 'absent' },
    estimated_persons: 0,
  });
  assert.equal(verdict.presence, true);
  assert.equal(verdict.persons, 1);
  assert.equal(verdict.authority, 'calibrated_occupancy');
  assert.equal(verdict.modelId, 'cal-model-1');
  assert.equal(verdict.calibrated, true);
  assert.equal(verdict.dutyShare, 0.92);
  assert.equal(verdict.dutyWindowMs, 30000);
});

test('a motion-only verdict keeps presence and claims no count', () => {
  const verdict = presenceEvidenceOf({
    calibrated_presence_evidence: {
      presence: true,
      person_count: 0,
      presence_authority: 'motion_evidence',
      inference_method: 'motion_evidence',
    },
  });
  assert.equal(verdict.presence, true);
  assert.equal(verdict.persons, 0, 'zero means no count is claimed');
  assert.equal(verdict.authority, 'motion_evidence');
});

test('a frame without evidence falls back to the classification verdict', () => {
  const verdict = presenceEvidenceOf({
    classification: { presence: true, motion_level: 'present_still' },
    estimated_persons: 1,
  });
  assert.equal(verdict.presence, true);
  assert.equal(verdict.persons, 1);
  assert.equal(verdict.calibrated, false);
  assert.equal(verdict.authority, 'classification');
});

test('a malformed or empty frame reports no verdict instead of throwing', () => {
  for (const message of [null, undefined, {}, { classification: {} }]) {
    const verdict = presenceEvidenceOf(message);
    assert.equal(verdict.presence, null);
    assert.equal(verdict.persons, 0);
    assert.equal(verdict.calibrated, false);
  }
});

test('an evidence object without a count still reports its verdict', () => {
  const verdict = presenceEvidenceOf({
    calibrated_presence_evidence: { presence: false, presence_authority: 'none' },
  });
  assert.equal(verdict.presence, false);
  assert.equal(verdict.persons, 0);
  assert.equal(verdict.authority, 'none');
});

test('authority labels are human readable and never undefined', () => {
  assert.equal(presenceAuthorityLabel('calibrated_occupancy'), 'calibrated room model');
  assert.equal(presenceAuthorityLabel('motion_evidence'), 'motion evidence (no count)');
  assert.equal(presenceAuthorityLabel('classification'), 'room classification');
  assert.equal(presenceAuthorityLabel(undefined), 'unknown');
});
