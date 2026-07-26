export interface RevisionedDraft {
  draft_revision: number;
  summary: string;
}

export type RefreshEpoch = {
  request: number;
  state: number;
};

type Options<T extends RevisionedDraft> = {
  saveSummary: (draft: T | null, summary: string) => Promise<T | null>;
  onDraft: (draft: T | null) => void;
  onSummary: (summary: string, dirty: boolean) => void;
};

/**
 * The single command owner for one private review draft. Every mutation enters
 * the promise tail and receives the newest committed draft revision. Refreshes
 * are admitted only when neither local text nor a queued command advanced
 * since their request began.
 */
export class ReviewDraftController<T extends RevisionedDraft> {
  private readonly options: Options<T>;
  private current: T | null = null;
  private summary = '';
  private persistedSummary = '';
  private dirty = false;
  private frozen = false;
  private state = 0;
  private refreshRequest = 0;
  private tail: Promise<void> = Promise.resolve();

  constructor(options: Options<T>) {
    this.options = options;
  }

  get draft(): T | null {
    return this.current;
  }

  get summaryDirty(): boolean {
    return this.dirty;
  }

  hydrate(draft: T | null): void {
    this.current = draft;
    this.summary = draft?.summary ?? '';
    this.persistedSummary = this.summary;
    this.dirty = false;
    this.state += 1;
    this.options.onDraft(draft);
    this.options.onSummary(this.summary, false);
  }

  reconcile(draft: T): void {
    this.current = draft;
    this.state += 1;
    this.options.onDraft(draft);
    if (!this.dirty) {
      this.summary = draft.summary;
      this.persistedSummary = draft.summary;
      this.options.onSummary(this.summary, false);
    }
  }

  editSummary(summary: string): void {
    this.summary = summary;
    this.dirty = summary !== this.persistedSummary;
    this.state += 1;
    this.options.onSummary(summary, this.dirty);
  }

  beginRefresh(): RefreshEpoch {
    return { request: ++this.refreshRequest, state: this.state };
  }

  acceptRefresh(epoch: RefreshEpoch, draft: T | null): boolean {
    if (epoch.request !== this.refreshRequest || epoch.state !== this.state || this.dirty) {
      return false;
    }
    this.hydrate(draft);
    return true;
  }

  command(run: (draft: T | null) => Promise<T>): Promise<T> {
    if (this.frozen) return Promise.reject(new Error('review draft is finalizing'));
    this.state += 1;
    return this.schedule(async () => {
      await this.persistDirtySummary();
      const next = await run(this.current);
      this.setCurrent(next);
      return next;
    });
  }

  flush(): Promise<T | null> {
    return this.schedule(async () => {
      while (this.dirty) await this.persistDirtySummary();
      return this.current;
    });
  }

  async barrier(): Promise<() => void> {
    if (this.frozen) throw new Error('review draft is already finalizing');
    this.frozen = true;
    this.state += 1;
    let held = true;
    const release = () => {
      if (!held) return;
      held = false;
      this.frozen = false;
    };
    try {
      await this.schedule(async () => {
        while (this.dirty) await this.persistDirtySummary();
      });
      return release;
    } catch (error) {
      release();
      throw error;
    }
  }

  async freeze<R>(run: (draft: T | null) => Promise<{ draft: T | null; result: R }>): Promise<R> {
    if (this.frozen) throw new Error('review draft is already finalizing');
    this.frozen = true;
    this.state += 1;
    try {
      const settled = await this.schedule(async () => {
        while (this.dirty) await this.persistDirtySummary();
        const outcome = await run(this.current);
        this.setCurrent(outcome.draft);
        return outcome.result;
      });
      return settled;
    } finally {
      this.frozen = false;
    }
  }

  private schedule<R>(run: () => Promise<R>): Promise<R> {
    const result = this.tail.then(run);
    this.tail = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }

  private async persistDirtySummary(): Promise<void> {
    if (!this.dirty) return;
    const snapshot = this.summary;
    const next = await this.options.saveSummary(this.current, snapshot);
    this.setCurrent(next);
    this.persistedSummary = snapshot;
    this.dirty = this.summary !== snapshot;
    this.options.onSummary(this.summary, this.dirty);
  }

  private setCurrent(draft: T | null): void {
    this.current = draft;
    this.state += 1;
    this.options.onDraft(draft);
    if (!draft) {
      this.summary = '';
      this.persistedSummary = '';
      this.dirty = false;
      this.options.onSummary('', false);
    }
  }
}
