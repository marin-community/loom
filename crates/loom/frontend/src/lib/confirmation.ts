import { reactive } from 'vue';

export interface ConfirmationOptions {
  title: string;
  description: string;
  confirmLabel: string;
  action: () => Promise<void>;
  danger?: boolean;
}

export const confirmation = reactive({
  open: false,
  title: '',
  description: '',
  confirmLabel: '',
  danger: false,
  busy: false,
  error: '',
});

let action: (() => Promise<void>) | null = null;
let settle: ((confirmed: boolean) => void) | null = null;

function close(confirmed: boolean) {
  confirmation.open = false;
  confirmation.busy = false;
  confirmation.error = '';
  action = null;
  settle?.(confirmed);
  settle = null;
}

export function confirmAction(options: ConfirmationOptions): Promise<boolean> {
  if (confirmation.open) return Promise.reject(new Error('another confirmation is already open'));
  confirmation.title = options.title;
  confirmation.description = options.description;
  confirmation.confirmLabel = options.confirmLabel;
  confirmation.danger = options.danger ?? false;
  confirmation.busy = false;
  confirmation.error = '';
  confirmation.open = true;
  action = options.action;
  return new Promise((resolve) => {
    settle = resolve;
  });
}

export function cancelConfirmation() {
  if (!confirmation.busy) close(false);
}

export async function acceptConfirmation() {
  if (!action || confirmation.busy) return;
  confirmation.busy = true;
  confirmation.error = '';
  try {
    await action();
    close(true);
  } catch (cause) {
    confirmation.error = (cause as Error).message;
    confirmation.busy = false;
  }
}
