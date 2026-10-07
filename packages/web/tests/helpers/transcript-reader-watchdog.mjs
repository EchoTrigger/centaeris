// A timer alone cannot stop an infinite microtask loop. Bound consecutive
// no-progress calls so the unfixed reader fails deterministically instead.
export function watchTranscriptLoads(Reader) {
  const loadMore = Reader.prototype.loadMore;
  const loadAll = Reader.prototype.loadAll;
  const idleCalls = new WeakMap();
  const operations = [];
  Reader.prototype.loadMore = async function () {
    const before = this.getSnapshot();
    await loadMore.call(this);
    const idle = before === this.getSnapshot() ? (idleCalls.get(this) ?? 0) + 1 : 0;
    idleCalls.set(this, idle);
    if (idle > 8) throw new Error("transcript reader exceeded bounded no-progress watchdog");
  };
  Reader.prototype.loadAll = function () {
    const operation = { reader: this, settled: false, error: null, pending: null };
    const pending = loadAll.call(this);
    operation.pending = pending.then(
      () => { operation.settled = true; },
      (error) => { operation.settled = true; operation.error = String(error); },
    );
    operations.push(operation);
    return pending;
  };
  return { operations, restore() {
    Reader.prototype.loadMore = loadMore;
    Reader.prototype.loadAll = loadAll;
  } };
}
