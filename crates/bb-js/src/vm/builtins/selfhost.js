"use strict";
// Built-ins written in JavaScript; see selfhost.rs. `I` holds native helpers and
// intrinsics. Never call methods of built-in prototypes, use spread or for…of here:
// user code can patch those.
(function (I) {
  const TypeError = I.TypeError;
  const isObject = (v) => (typeof v === "object" && v !== null) || typeof v === "function";

  // GetMethod (§7.3.10).
  function getMethod(v, k) {
    const f = v[k];
    if (f === undefined || f === null) return undefined;
    if (!I.isCallable(f)) throw new TypeError("not a function");
    return f;
  }

  // Array.fromAsync (proposal-array-from-async, §2.1.1.1).
  I.defineMethod(I.Array, "fromAsync", async function fromAsync(asyncItems, mapfn = undefined, thisArg = undefined) {
    const C = this;
    let mapping = false;
    if (mapfn !== undefined) {
      if (!I.isCallable(mapfn)) throw new TypeError("Array.fromAsync: the mapper is not a function");
      mapping = true;
    }
    const usingAsyncIterator = getMethod(asyncItems, I.asyncIterator);
    const usingSyncIterator = usingAsyncIterator === undefined ? getMethod(asyncItems, I.iterator) : undefined;
    if (usingAsyncIterator !== undefined || usingSyncIterator !== undefined) {
      let iterator;
      if (usingAsyncIterator !== undefined) {
        iterator = I.call(usingAsyncIterator, asyncItems);
        if (!isObject(iterator)) throw new TypeError("the iterator is not an object");
      } else {
        const sync = I.call(usingSyncIterator, asyncItems);
        if (!isObject(sync)) throw new TypeError("the iterator is not an object");
        iterator = I.asyncFromSyncIterator(sync, sync.next);
      }
      const next = iterator.next;
      const A = I.isConstructor(C) ? I.construct(C) : I.arrayCreate(0);
      let k = 0;
      for (;;) {
        if (k >= 9007199254740991) {
          const error = new TypeError("Array.fromAsync: too many elements");
          try {
            const ret = iterator.return;
            if (ret !== undefined && ret !== null) await I.call(ret, iterator);
          } catch (_) {}
          throw error;
        }
        const result = await I.call(next, iterator);
        if (!isObject(result)) throw new TypeError("the iterator result is not an object");
        if (result.done) {
          A.length = k;
          return A;
        }
        const value = result.value;
        let mapped = value;
        try {
          if (mapping) mapped = await I.call(mapfn, thisArg, value, k);
          I.createDataPropertyOrThrow(A, k, mapped);
        } catch (error) {
          // AsyncIteratorClose with a throw completion: its own errors are dropped.
          try {
            const ret = iterator.return;
            if (ret !== undefined && ret !== null) await I.call(ret, iterator);
          } catch (_) {}
          throw error;
        }
        k++;
      }
    }
    const arrayLike = I.toObject(asyncItems);
    const len = I.lengthOfArrayLike(arrayLike);
    const A = I.isConstructor(C) ? I.construct(C, len) : I.arrayCreate(len);
    let k = 0;
    while (k < len) {
      let value = await arrayLike[k];
      if (mapping) value = await I.call(mapfn, thisArg, value, k);
      I.createDataPropertyOrThrow(A, k, value);
      k++;
    }
    A.length = len;
    return A;
  });

  // AsyncDisposableStack.prototype.disposeAsync (§27.4.3.5): DisposeResources (§9.5.7)
  // over resources that are all async-dispose.
  async function disposeAsync() {
    const resources = I.asyncStackTake(this);
    if (resources === undefined) return undefined;
    let hasError = false;
    let error;
    let needsAwait = false;
    let hasAwaited = false;
    for (let i = 0; i < resources.length; i += 2) {
      const value = resources[i];
      const method = resources[i + 1];
      if (method === undefined) {
        needsAwait = true;
        continue;
      }
      try {
        const result = I.call(method, value);
        hasAwaited = true;
        await result;
      } catch (e) {
        if (hasError) {
          error = I.suppressedError(e, error);
        } else {
          hasError = true;
          error = e;
        }
      }
    }
    if (needsAwait && !hasAwaited) await undefined;
    if (hasError) throw error;
    return undefined;
  }
  I.defineMethod(I.AsyncDisposableStackPrototype, "disposeAsync", disposeAsync);
  I.defineMethod(I.AsyncDisposableStackPrototype, I.asyncDispose, disposeAsync);

  // %AsyncIteratorPrototype% [ @@asyncDispose ] (§27.1.3.1).
  I.defineMethod(I.AsyncIteratorPrototype, I.asyncDispose, async function () {
    const ret = getMethod(this, "return");
    if (ret !== undefined) await I.call(ret, this);
    return undefined;
  });
});
