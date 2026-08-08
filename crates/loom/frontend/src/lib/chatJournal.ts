export interface JournalPosition {
  turn: number;
  seq: number;
}

/**
 * Reconciles the REST journal snapshot with committed blocks arriving over SSE.
 *
 * A snapshot request and the stream run concurrently. If a mutable block (for
 * example a resolved permission) arrives after the request begins, the older
 * response must not overwrite it. A later snapshot is allowed to advance it
 * again, which repairs any stream gap. SQLite remains the authority; this class
 * only preserves the observed ordering of two views onto that authority.
 */
export class ChatJournalReconciler<T extends JournalPosition> {
  private revision = 0;
  private resetAt = 0;
  private readonly streamedAt = new Map<string, number>();
  private readonly blocks: Map<string, T>;

  constructor(blocks: Map<string, T>) {
    this.blocks = blocks;
  }

  static key(block: JournalPosition): string {
    return `${block.turn}:${block.seq}`;
  }

  reset(): void {
    this.blocks.clear();
    this.streamedAt.clear();
    this.resetAt = ++this.revision;
  }

  beginSnapshot(): number {
    return this.revision;
  }

  applyStream(block: T): void {
    const key = ChatJournalReconciler.key(block);
    this.blocks.set(key, block);
    this.streamedAt.set(key, ++this.revision);
  }

  /** Apply every snapshot block that was not superseded by a stream event
   * observed while this request was in flight. Returns the accepted blocks so
   * the caller can settle matching streaming shadows and live tool state. */
  applySnapshot(startedAt: number, snapshot: T[]): T[] {
    const accepted: T[] = [];
    if (startedAt < this.resetAt) return accepted;
    for (const block of snapshot) {
      const key = ChatJournalReconciler.key(block);
      if ((this.streamedAt.get(key) ?? 0) > startedAt) continue;
      this.blocks.set(key, block);
      accepted.push(block);
    }
    return accepted;
  }
}
