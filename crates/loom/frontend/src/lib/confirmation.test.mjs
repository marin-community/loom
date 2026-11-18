import test from 'node:test';
import assert from 'node:assert/strict';
import {
  acceptConfirmation,
  cancelConfirmation,
  confirmAction,
  confirmation,
} from './confirmation.ts';

// confirmAction's promise settles only when the dialog closes, so each dialog
// is closed (cancelled) before its promise is awaited.
const cancelledDialog = (option) => {
  const settled = confirmAction({
    title: 'Archive this session?',
    description: 'd',
    confirmLabel: 'Archive session',
    option,
    action: async () => {},
  });
  cancelConfirmation();
  return settled;
};

test('the optional add-on seeds its checkbox from the option', async () => {
  await cancelledDialog();
  assert.equal(confirmation.optionLabel, '');

  await cancelledDialog({ label: 'Also archive child sessions' });
  assert.equal(confirmation.optionChecked, false, 'the default tick is unticked');

  await cancelledDialog({ label: 'Also archive child sessions', checked: true });
  assert.equal(confirmation.optionChecked, false, 'closing resets the tick for the next dialog');

  const settled = confirmAction({
    title: 'Archive this session?',
    description: 'd',
    confirmLabel: 'Archive session',
    option: { label: 'Also archive child sessions', checked: true },
    action: async ({ optionChecked }) => {
      assert.equal(optionChecked, true, 'the seeded tick reaches the action');
    },
  });
  await acceptConfirmation();
  await settled;
  assert.equal(confirmation.open, false);
});
