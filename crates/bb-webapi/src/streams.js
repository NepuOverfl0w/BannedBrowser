"use strict";
// The Streams Standard (https://streams.spec.whatwg.org/) in JavaScript, run once per realm
// on first use (see streams.rs). Section numbers are the standard's. Byte streams
// (ReadableByteStreamController, BYOB readers) aren't done: `type: "bytes"` throws.
(function (global) {
  const Promise = global.Promise;
  const TypeError = global.TypeError;
  const RangeError = global.RangeError;
  const Symbol_ = global.Symbol;
  const ObjectDefineProperty = global.Object.defineProperty;
  const ObjectGetOwnPropertyNames = global.Object.getOwnPropertyNames;
  const ObjectGetOwnPropertyDescriptor = global.Object.getOwnPropertyDescriptor;
  const ReflectApply = global.Reflect.apply;
  const PromiseThen = Promise.prototype.then;
  const PromiseResolve = Promise.resolve.bind(Promise);
  const PromiseReject = Promise.reject.bind(Promise);
  const queueMicrotask = global.queueMicrotask;
  const NumberIsNaN = global.Number.isNaN;
  const ArrayBuffer_ = global.ArrayBuffer;
  const Uint8Array_ = global.Uint8Array;
  const AsyncIteratorPrototype = Object.getPrototypeOf(Object.getPrototypeOf(async function* () {}).prototype);

  const isObject = (v) => (typeof v === "object" && v !== null) || typeof v === "function";
  const call = (f, self, args) => ReflectApply(f, self, args);
  const then = (p, onOk, onErr) => ReflectApply(PromiseThen, p, [onOk, onErr]);
  const ignore = (p) => {
    then(p, undefined, () => {});
  };

  // A promise with its resolving functions, and whether it settled (a pending check).
  function newPromise() {
    const d = { promise: undefined, resolve: undefined, reject: undefined, settled: false };
    d.promise = new Promise((a, b) => {
      d.resolve = (v) => {
        if (!d.settled) {
          d.settled = true;
          a(v);
        }
      };
      d.reject = (e) => {
        if (!d.settled) {
          d.settled = true;
          b(e);
        }
      };
    });
    return d;
  }

  // "upon fulfillment / rejection" helpers.
  const react = (p, ok, err) => then(p, ok, err);
  // "promise resolved with" a value, through the realm's Promise.
  const resolved = (v) => PromiseResolve(v);
  const rejected = (e) => PromiseReject(e);

  // ---- queuing strategies (§7) ----

  // §7.4 ExtractHighWaterMark, ExtractSizeAlgorithm.
  function extractHighWaterMark(strategy, defaultHWM) {
    const hwm = strategy.highWaterMark;
    if (hwm === undefined) return defaultHWM;
    const n = +hwm;
    if (NumberIsNaN(n) || n < 0) throw new RangeError("Invalid highWaterMark");
    return n;
  }
  function extractSizeAlgorithm(strategy) {
    const size = strategy.size;
    if (size === undefined) return () => 1;
    if (typeof size !== "function") throw new TypeError("size must be a function");
    return (chunk) => call(size, undefined, [chunk]);
  }
  // A QueuingStrategy dictionary from an object (WebIDL conversion).
  function strategyDict(s) {
    if (s === undefined || s === null) return {};
    if (!isObject(s)) throw new TypeError("the strategy must be an object");
    const hwm = s.highWaterMark;
    const size = s.size;
    if (size !== undefined && typeof size !== "function") throw new TypeError("size must be a function");
    return { highWaterMark: hwm === undefined ? undefined : +hwm, size };
  }

  const byteLengthSize = function size(chunk) {
    return chunk.byteLength;
  };
  const countSize = function size() {
    return 1;
  };

  class ByteLengthQueuingStrategy {
    #hwm;
    constructor(init) {
      if (!isObject(init)) throw new TypeError("ByteLengthQueuingStrategy needs an init dictionary");
      const hwm = init.highWaterMark;
      if (hwm === undefined) throw new TypeError("highWaterMark is required");
      this.#hwm = +hwm;
    }
    get highWaterMark() {
      if (!(#hwm in this)) throw new TypeError("Illegal invocation");
      return this.#hwm;
    }
    get size() {
      if (!(#hwm in this)) throw new TypeError("Illegal invocation");
      return byteLengthSize;
    }
  }

  class CountQueuingStrategy {
    #hwm;
    constructor(init) {
      if (!isObject(init)) throw new TypeError("CountQueuingStrategy needs an init dictionary");
      const hwm = init.highWaterMark;
      if (hwm === undefined) throw new TypeError("highWaterMark is required");
      this.#hwm = +hwm;
    }
    get highWaterMark() {
      if (!(#hwm in this)) throw new TypeError("Illegal invocation");
      return this.#hwm;
    }
    get size() {
      if (!(#hwm in this)) throw new TypeError("Illegal invocation");
      return countSize;
    }
  }

  // ---- queue-with-sizes (§8.1) ----

  function dequeueValue(c) {
    const pair = c.queue.shift();
    c.queueTotalSize -= pair.size;
    if (c.queueTotalSize < 0) c.queueTotalSize = 0;
    return pair.value;
  }
  function enqueueValueWithSize(c, value, size) {
    if (typeof size !== "number" || NumberIsNaN(size) || size < 0 || size === Infinity) {
      throw new RangeError("Invalid chunk size");
    }
    c.queue.push({ value, size });
    c.queueTotalSize += size;
  }
  function peekQueueValue(c) {
    return c.queue[0].value;
  }
  function resetQueue(c) {
    c.queue = [];
    c.queueTotalSize = 0;
  }

  // Internal state lives in objects behind private fields; `slots` reaches them.
  const RS = new WeakMap(); // ReadableStream -> state
  const RSDC = new WeakMap(); // ReadableStreamDefaultController -> state
  const RSDR = new WeakMap(); // ReadableStreamDefaultReader -> state
  const WS = new WeakMap();
  const WSDC = new WeakMap();
  const WSDW = new WeakMap();
  const TS = new WeakMap();
  const TSDC = new WeakMap();
  const internal = Symbol_("internal");

  function slot(map, o, what) {
    const s = isObject(o) ? map.get(o) : undefined;
    if (s === undefined) throw new TypeError("Illegal invocation: not a " + what);
    return s;
  }

  // ---- ReadableStream (§4.2) ----

  class ReadableStream {
    constructor(underlyingSource = undefined, strategy = undefined) {
      if (underlyingSource === null) throw new TypeError("underlyingSource can't be null");
      const source = underlyingSource === undefined ? {} : underlyingSource;
      if (!isObject(source)) throw new TypeError("underlyingSource must be an object");
      const strat = strategyDict(strategy);
      const s = initializeReadableStream(this);
      const type = source.type;
      if (type !== undefined && `${type}` === "bytes") {
        throw new RangeError("readable byte streams aren't supported yet");
      } else if (type !== undefined) {
        throw new TypeError("type must be undefined or 'bytes'");
      }
      const sizeAlgorithm = extractSizeAlgorithm(strat);
      const highWaterMark = extractHighWaterMark(strat, 1);
      setUpReadableStreamDefaultControllerFromUnderlyingSource(s, source, highWaterMark, sizeAlgorithm);
    }
    static from(asyncIterable) {
      return readableStreamFromIterable(asyncIterable);
    }
    get locked() {
      return isReadableStreamLocked(slot(RS, this, "ReadableStream"));
    }
    cancel(reason = undefined) {
      let s;
      try {
        s = slot(RS, this, "ReadableStream");
      } catch (e) {
        return rejected(e);
      }
      if (isReadableStreamLocked(s)) return rejected(new TypeError("the stream is locked"));
      return readableStreamCancel(s, reason);
    }
    getReader(options = undefined) {
      const s = slot(RS, this, "ReadableStream");
      const mode = options === undefined || options === null ? undefined : options.mode;
      if (mode === undefined) return acquireReadableStreamDefaultReader(s);
      if (`${mode}` === "byob") throw new TypeError("BYOB readers need a byte stream (not supported yet)");
      throw new TypeError("mode must be 'byob'");
    }
    pipeThrough(transform, options = undefined) {
      const s = slot(RS, this, "ReadableStream");
      if (!isObject(transform)) throw new TypeError("pipeThrough needs a transform pair");
      const readable = transform.readable;
      const rs = slot(RS, readable, "ReadableStream");
      const writable = transform.writable;
      const ws = slot(WS, writable, "WritableStream");
      const o = pipeOptions(options);
      if (isReadableStreamLocked(s)) throw new TypeError("the stream is locked");
      if (isWritableStreamLocked(ws)) throw new TypeError("the writable stream is locked");
      const p = readableStreamPipeTo(s, ws, o.preventClose, o.preventAbort, o.preventCancel, o.signal);
      ignore(p);
      return readable;
    }
    pipeTo(destination, options = undefined) {
      let s, ws, o;
      try {
        s = slot(RS, this, "ReadableStream");
        ws = slot(WS, destination, "WritableStream");
        o = pipeOptions(options);
      } catch (e) {
        return rejected(e);
      }
      if (isReadableStreamLocked(s)) return rejected(new TypeError("the stream is locked"));
      if (isWritableStreamLocked(ws)) return rejected(new TypeError("the destination is locked"));
      return readableStreamPipeTo(s, ws, o.preventClose, o.preventAbort, o.preventCancel, o.signal);
    }
    tee() {
      const s = slot(RS, this, "ReadableStream");
      const [a, b] = readableStreamDefaultTee(s, false);
      return [a.object, b.object];
    }
    values(options = undefined) {
      const s = slot(RS, this, "ReadableStream");
      const preventCancel = options !== undefined && options !== null && !!options.preventCancel;
      return createAsyncIterator(s, preventCancel);
    }
  }

  function pipeOptions(options) {
    if (options === undefined || options === null) {
      return { preventClose: false, preventAbort: false, preventCancel: false, signal: undefined };
    }
    if (!isObject(options)) throw new TypeError("options must be an object");
    const preventAbort = !!options.preventAbort;
    const preventCancel = !!options.preventCancel;
    const preventClose = !!options.preventClose;
    const signal = options.signal;
    if (signal !== undefined && !(isObject(signal) && "aborted" in signal && typeof signal.addEventListener === "function")) {
      throw new TypeError("signal must be an AbortSignal");
    }
    return { preventClose, preventAbort, preventCancel, signal };
  }

  function initializeReadableStream(stream) {
    const s = {
      object: stream,
      state: "readable",
      reader: undefined,
      storedError: undefined,
      disturbed: false,
      controller: undefined,
    };
    RS.set(stream, s);
    return s;
  }

  // §4.9.1 working with readable streams.
  function createReadableStream(startAlgorithm, pullAlgorithm, cancelAlgorithm, highWaterMark = 1, sizeAlgorithm = () => 1) {
    const stream = Object.create(ReadableStream.prototype);
    const s = initializeReadableStream(stream);
    const c = newDefaultController();
    setUpReadableStreamDefaultController(s, c, startAlgorithm, pullAlgorithm, cancelAlgorithm, highWaterMark, sizeAlgorithm);
    return s;
  }

  function isReadableStreamLocked(s) {
    return s.reader !== undefined;
  }

  function readableStreamCancel(s, reason) {
    s.disturbed = true;
    if (s.state === "closed") return resolved(undefined);
    if (s.state === "errored") return rejected(s.storedError);
    readableStreamClose(s);
    const r = s.reader;
    if (r !== undefined && r.kind === "byob") {
      // (no BYOB readers yet)
    }
    const p = s.controller.cancelSteps(reason);
    return react(p, () => undefined);
  }

  function readableStreamClose(s) {
    s.state = "closed";
    const r = s.reader;
    if (r === undefined) return;
    r.closed.resolve(undefined);
    const requests = r.readRequests;
    r.readRequests = [];
    for (let i = 0; i < requests.length; i++) requests[i].close();
  }

  function readableStreamError(s, e) {
    s.state = "errored";
    s.storedError = e;
    const r = s.reader;
    if (r === undefined) return;
    r.closed.reject(e);
    ignore(r.closed.promise);
    const requests = r.readRequests;
    r.readRequests = [];
    for (let i = 0; i < requests.length; i++) requests[i].error(e);
  }

  function readableStreamAddReadRequest(s, req) {
    s.reader.readRequests.push(req);
  }

  function readableStreamFulfillReadRequest(s, chunk, done) {
    const r = s.reader;
    const req = r.readRequests.shift();
    if (done) req.close();
    else req.chunk(chunk);
  }

  function readableStreamGetNumReadRequests(s) {
    return s.reader.readRequests.length;
  }

  function readableStreamHasDefaultReader(s) {
    return s.reader !== undefined && s.reader.kind === "default";
  }

  // ---- readers (§4.3) ----

  class ReadableStreamDefaultReader {
    constructor(stream) {
      const s = slot(RS, stream, "ReadableStream");
      setUpReadableStreamDefaultReader(this, s);
    }
    get closed() {
      return slot(RSDR, this, "ReadableStreamDefaultReader").closed.promise;
    }
    cancel(reason = undefined) {
      let r;
      try {
        r = slot(RSDR, this, "ReadableStreamDefaultReader");
      } catch (e) {
        return rejected(e);
      }
      if (r.stream === undefined) return rejected(new TypeError("the reader is released"));
      return readableStreamCancel(r.stream, reason);
    }
    read() {
      let r;
      try {
        r = slot(RSDR, this, "ReadableStreamDefaultReader");
      } catch (e) {
        return rejected(e);
      }
      if (r.stream === undefined) return rejected(new TypeError("the reader is released"));
      const p = newPromise();
      readableStreamDefaultReaderRead(r, {
        chunk: (value) => p.resolve({ value, done: false }),
        close: () => p.resolve({ value: undefined, done: true }),
        error: (e) => p.reject(e),
      });
      return p.promise;
    }
    releaseLock() {
      const r = slot(RSDR, this, "ReadableStreamDefaultReader");
      if (r.stream === undefined) return;
      readableStreamDefaultReaderRelease(r);
    }
  }

  function setUpReadableStreamDefaultReader(reader, s) {
    if (isReadableStreamLocked(s)) throw new TypeError("the stream is locked");
    const r = { kind: "default", object: reader, stream: undefined, closed: undefined, readRequests: [] };
    RSDR.set(reader, r);
    readableStreamReaderGenericInitialize(r, s);
    return r;
  }

  function acquireReadableStreamDefaultReader(s) {
    const reader = Object.create(ReadableStreamDefaultReader.prototype);
    setUpReadableStreamDefaultReader(reader, s);
    return reader;
  }

  function readableStreamReaderGenericInitialize(r, s) {
    r.stream = s;
    s.reader = r;
    r.closed = newPromise();
    if (s.state === "closed") r.closed.resolve(undefined);
    else if (s.state === "errored") {
      r.closed.reject(s.storedError);
      ignore(r.closed.promise);
    }
  }

  function readableStreamReaderGenericRelease(r) {
    const s = r.stream;
    r.closed = rejectOrReplace(r.closed, new TypeError("the reader was released"));
    s.controller.releaseSteps();
    s.reader = undefined;
    r.stream = undefined;
  }

  function readableStreamDefaultReaderRelease(r) {
    readableStreamReaderGenericRelease(r);
    const e = new TypeError("the reader was released");
    const requests = r.readRequests;
    r.readRequests = [];
    for (let i = 0; i < requests.length; i++) requests[i].error(e);
  }

  function readableStreamDefaultReaderRead(r, req) {
    const s = r.stream;
    s.disturbed = true;
    if (s.state === "closed") req.close();
    else if (s.state === "errored") req.error(s.storedError);
    else s.controller.pullSteps(req);
  }

  // ---- ReadableStreamDefaultController (§4.6) ----

  class ReadableStreamDefaultController {
    constructor() {
      throw new TypeError("Illegal constructor");
    }
    get desiredSize() {
      return readableStreamDefaultControllerGetDesiredSize(slot(RSDC, this, "ReadableStreamDefaultController"));
    }
    close() {
      const c = slot(RSDC, this, "ReadableStreamDefaultController");
      if (!readableStreamDefaultControllerCanCloseOrEnqueue(c)) throw new TypeError("the stream can't be closed");
      readableStreamDefaultControllerClose(c);
    }
    enqueue(chunk = undefined) {
      const c = slot(RSDC, this, "ReadableStreamDefaultController");
      if (!readableStreamDefaultControllerCanCloseOrEnqueue(c)) throw new TypeError("the stream can't be enqueued to");
      readableStreamDefaultControllerEnqueue(c, chunk);
    }
    error(e = undefined) {
      readableStreamDefaultControllerError(slot(RSDC, this, "ReadableStreamDefaultController"), e);
    }
  }

  function newDefaultController() {
    const object = Object.create(ReadableStreamDefaultController.prototype);
    const c = {
      object,
      stream: undefined,
      queue: [],
      queueTotalSize: 0,
      started: false,
      closeRequested: false,
      pullAgain: false,
      pulling: false,
      strategySizeAlgorithm: undefined,
      strategyHWM: 0,
      pullAlgorithm: undefined,
      cancelAlgorithm: undefined,
    };
    c.cancelSteps = (reason) => {
      resetQueue(c);
      const result = c.cancelAlgorithm(reason);
      readableStreamDefaultControllerClearAlgorithms(c);
      return result;
    };
    c.pullSteps = (req) => {
      const s = c.stream;
      if (c.queue.length > 0) {
        const chunk = dequeueValue(c);
        if (c.closeRequested && c.queue.length === 0) {
          readableStreamDefaultControllerClearAlgorithms(c);
          readableStreamClose(s);
        } else readableStreamDefaultControllerCallPullIfNeeded(c);
        req.chunk(chunk);
      } else {
        readableStreamAddReadRequest(s, req);
        readableStreamDefaultControllerCallPullIfNeeded(c);
      }
    };
    c.releaseSteps = () => {};
    RSDC.set(object, c);
    return c;
  }

  function readableStreamDefaultControllerCallPullIfNeeded(c) {
    if (!readableStreamDefaultControllerShouldCallPull(c)) return;
    if (c.pulling) {
      c.pullAgain = true;
      return;
    }
    c.pulling = true;
    react(
      c.pullAlgorithm(),
      () => {
        c.pulling = false;
        if (c.pullAgain) {
          c.pullAgain = false;
          readableStreamDefaultControllerCallPullIfNeeded(c);
        }
      },
      (e) => readableStreamDefaultControllerError(c, e)
    );
  }

  function readableStreamDefaultControllerShouldCallPull(c) {
    const s = c.stream;
    if (!readableStreamDefaultControllerCanCloseOrEnqueue(c)) return false;
    if (!c.started) return false;
    if (isReadableStreamLocked(s) && readableStreamGetNumReadRequests(s) > 0) return true;
    return readableStreamDefaultControllerGetDesiredSize(c) > 0;
  }

  function readableStreamDefaultControllerClearAlgorithms(c) {
    c.pullAlgorithm = undefined;
    c.cancelAlgorithm = undefined;
    c.strategySizeAlgorithm = undefined;
  }

  function readableStreamDefaultControllerClose(c) {
    if (!readableStreamDefaultControllerCanCloseOrEnqueue(c)) return;
    const s = c.stream;
    c.closeRequested = true;
    if (c.queue.length === 0) {
      readableStreamDefaultControllerClearAlgorithms(c);
      readableStreamClose(s);
    }
  }

  function readableStreamDefaultControllerEnqueue(c, chunk) {
    if (!readableStreamDefaultControllerCanCloseOrEnqueue(c)) return;
    const s = c.stream;
    if (isReadableStreamLocked(s) && readableStreamGetNumReadRequests(s) > 0) {
      readableStreamFulfillReadRequest(s, chunk, false);
    } else {
      let size;
      try {
        size = c.strategySizeAlgorithm(chunk);
      } catch (e) {
        readableStreamDefaultControllerError(c, e);
        throw e;
      }
      try {
        enqueueValueWithSize(c, chunk, size);
      } catch (e) {
        readableStreamDefaultControllerError(c, e);
        throw e;
      }
    }
    readableStreamDefaultControllerCallPullIfNeeded(c);
  }

  function readableStreamDefaultControllerError(c, e) {
    const s = c.stream;
    if (s.state !== "readable") return;
    resetQueue(c);
    readableStreamDefaultControllerClearAlgorithms(c);
    readableStreamError(s, e);
  }

  function readableStreamDefaultControllerGetDesiredSize(c) {
    const state = c.stream.state;
    if (state === "errored") return null;
    if (state === "closed") return 0;
    return c.strategyHWM - c.queueTotalSize;
  }

  function readableStreamDefaultControllerHasBackpressure(c) {
    return !readableStreamDefaultControllerShouldCallPull(c);
  }

  function readableStreamDefaultControllerCanCloseOrEnqueue(c) {
    return !c.closeRequested && c.stream.state === "readable";
  }

  function setUpReadableStreamDefaultController(s, c, startAlgorithm, pullAlgorithm, cancelAlgorithm, highWaterMark, sizeAlgorithm) {
    c.stream = s;
    resetQueue(c);
    c.started = c.closeRequested = c.pullAgain = c.pulling = false;
    c.strategySizeAlgorithm = sizeAlgorithm;
    c.strategyHWM = highWaterMark;
    c.pullAlgorithm = pullAlgorithm;
    c.cancelAlgorithm = cancelAlgorithm;
    s.controller = c;
    const startResult = startAlgorithm();
    react(
      resolved(startResult),
      () => {
        c.started = true;
        readableStreamDefaultControllerCallPullIfNeeded(c);
      },
      (e) => readableStreamDefaultControllerError(c, e)
    );
  }

  function setUpReadableStreamDefaultControllerFromUnderlyingSource(s, source, highWaterMark, sizeAlgorithm) {
    const c = newDefaultController();
    const start = source.start;
    const pull = source.pull;
    const cancel = source.cancel;
    if (start !== undefined && typeof start !== "function") throw new TypeError("start must be a function");
    if (pull !== undefined && typeof pull !== "function") throw new TypeError("pull must be a function");
    if (cancel !== undefined && typeof cancel !== "function") throw new TypeError("cancel must be a function");
    const startAlgorithm = start === undefined ? () => undefined : () => call(start, source, [c.object]);
    const pullAlgorithm =
      pull === undefined
        ? () => resolved(undefined)
        : () => {
            try {
              return resolved(call(pull, source, [c.object]));
            } catch (e) {
              return rejected(e);
            }
          };
    const cancelAlgorithm =
      cancel === undefined
        ? () => resolved(undefined)
        : (reason) => {
            try {
              return resolved(call(cancel, source, [reason]));
            } catch (e) {
              return rejected(e);
            }
          };
    setUpReadableStreamDefaultController(s, c, startAlgorithm, pullAlgorithm, cancelAlgorithm, highWaterMark, sizeAlgorithm);
  }

  // §4.9.1 ReadableStreamDefaultTee.
  function readableStreamDefaultTee(s, cloneForBranch2) {
    const reader = acquireReadableStreamDefaultReader(s);
    const r = RSDR.get(reader);
    let reading = false;
    let readAgain = false;
    let canceled1 = false;
    let canceled2 = false;
    let reason1, reason2;
    let branch1, branch2;
    const cancelPromise = newPromise();
    const pullAlgorithm = () => {
      if (reading) {
        readAgain = true;
        return resolved(undefined);
      }
      reading = true;
      readableStreamDefaultReaderRead(r, {
        chunk: (chunk) => {
          queueMicrotask(() => {
            readAgain = false;
            const chunk1 = chunk;
            const chunk2 = chunk;
            if (!canceled1) readableStreamDefaultControllerEnqueue(branch1.controller, chunk1);
            if (!canceled2) readableStreamDefaultControllerEnqueue(branch2.controller, chunk2);
            reading = false;
            if (readAgain) pullAlgorithm();
          });
        },
        close: () => {
          reading = false;
          if (!canceled1) readableStreamDefaultControllerClose(branch1.controller);
          if (!canceled2) readableStreamDefaultControllerClose(branch2.controller);
          if (!canceled1 || !canceled2) cancelPromise.resolve(undefined);
        },
        error: () => {
          reading = false;
        },
      });
      return resolved(undefined);
    };
    const cancel1 = (reason) => {
      canceled1 = true;
      reason1 = reason;
      if (canceled2) cancelPromise.resolve(readableStreamCancel(s, [reason1, reason2]));
      return cancelPromise.promise;
    };
    const cancel2 = (reason) => {
      canceled2 = true;
      reason2 = reason;
      if (canceled1) cancelPromise.resolve(readableStreamCancel(s, [reason1, reason2]));
      return cancelPromise.promise;
    };
    branch1 = createReadableStream(() => undefined, pullAlgorithm, cancel1);
    branch2 = createReadableStream(() => undefined, pullAlgorithm, cancel2);
    react(r.closed.promise, undefined, (e) => {
      readableStreamDefaultControllerError(branch1.controller, e);
      readableStreamDefaultControllerError(branch2.controller, e);
      if (!canceled1 || !canceled2) cancelPromise.resolve(undefined);
    });
    return [branch1, branch2];
  }

  // §4.2.5 ReadableStream.from.
  function readableStreamFromIterable(asyncIterable) {
    // WebIDL async_sequence: only objects convert.
    if (!isObject(asyncIterable)) throw new TypeError("ReadableStream.from needs an iterable object");
    let method = asyncIterable === null || asyncIterable === undefined ? undefined : asyncIterable[Symbol_.asyncIterator];
    let iterator;
    if (method === undefined || method === null) {
      const sync = asyncIterable === null || asyncIterable === undefined ? undefined : asyncIterable[Symbol_.iterator];
      if (typeof sync !== "function") throw new TypeError("the argument is not iterable");
      const syncIterator = call(sync, asyncIterable, []);
      if (!isObject(syncIterator)) throw new TypeError("the iterator is not an object");
      const next = syncIterator.next;
      iterator = {
        next() {
          try {
            const r = call(next, syncIterator, []);
            if (!isObject(r)) throw new TypeError("the iterator result is not an object");
            return then(resolved(r.value), (value) => ({ value, done: !!r.done }));
          } catch (e) {
            return rejected(e);
          }
        },
        return(v) {
          const ret = syncIterator.return;
          if (ret === undefined || ret === null) return resolved({ value: v, done: true });
          try {
            return resolved(call(ret, syncIterator, [v]));
          } catch (e) {
            return rejected(e);
          }
        },
      };
    } else {
      if (typeof method !== "function") throw new TypeError("Symbol.asyncIterator is not a function");
      iterator = call(method, asyncIterable, []);
      if (!isObject(iterator)) throw new TypeError("the iterator is not an object");
    }
    const nextMethod = iterator.next;
    let s;
    const pullAlgorithm = () => {
      let nextResult;
      try {
        nextResult = call(nextMethod, iterator, []);
      } catch (e) {
        return rejected(e);
      }
      return react(resolved(nextResult), (r) => {
        if (!isObject(r)) throw new TypeError("the iterator result is not an object");
        if (r.done) readableStreamDefaultControllerClose(s.controller);
        else readableStreamDefaultControllerEnqueue(s.controller, r.value);
      });
    };
    const cancelAlgorithm = (reason) => {
      let ret;
      try {
        ret = iterator.return;
      } catch (e) {
        return rejected(e);
      }
      if (ret === undefined || ret === null) return resolved(undefined);
      let r;
      try {
        r = call(ret, iterator, [reason]);
      } catch (e) {
        return rejected(e);
      }
      return react(resolved(r), (x) => {
        if (!isObject(x)) throw new TypeError("the iterator's return result is not an object");
      });
    };
    s = createReadableStream(() => undefined, pullAlgorithm, cancelAlgorithm, 0);
    return s.object;
  }

  // §4.2.6 async iteration.
  const ReadableStreamAsyncIteratorPrototype = Object.create(AsyncIteratorPrototype);
  const ITER = new WeakMap();
  function createAsyncIterator(s, preventCancel) {
    const reader = acquireReadableStreamDefaultReader(s);
    const it = Object.create(ReadableStreamAsyncIteratorPrototype);
    ITER.set(it, { reader: RSDR.get(reader), preventCancel, ongoing: undefined, finished: false });
    return it;
  }
  function iterNext(st) {
    if (st.finished) return resolved({ value: undefined, done: true });
    const r = st.reader;
    if (r.stream === undefined) return rejected(new TypeError("the iterator's reader was released"));
    const p = newPromise();
    readableStreamDefaultReaderRead(r, {
      chunk: (value) => p.resolve({ value, done: false }),
      close: () => {
        readableStreamDefaultReaderRelease(r);
        st.finished = true;
        p.resolve({ value: undefined, done: true });
      },
      error: (e) => {
        readableStreamDefaultReaderRelease(r);
        st.finished = true;
        p.reject(e);
      },
    });
    return p.promise;
  }
  ObjectDefineProperty(ReadableStreamAsyncIteratorPrototype, "next", {
    value: function next() {
      const st = ITER.get(this);
      if (st === undefined) return rejected(new TypeError("Illegal invocation"));
      const run = () => iterNext(st);
      st.ongoing = st.ongoing === undefined ? run() : then(st.ongoing, run, run);
      const mine = st.ongoing;
      then(mine, () => {
        if (st.ongoing === mine) st.ongoing = undefined;
      }, () => {
        if (st.ongoing === mine) st.ongoing = undefined;
      });
      return mine;
    },
    writable: true,
    enumerable: true,
    configurable: true,
  });
  ObjectDefineProperty(ReadableStreamAsyncIteratorPrototype, "return", {
    value: function return_(value) {
      const st = ITER.get(this);
      if (st === undefined) return rejected(new TypeError("Illegal invocation"));
      const run = () => {
        if (st.finished) return resolved({ value, done: true });
        st.finished = true;
        const r = st.reader;
        if (r.stream === undefined) return resolved({ value, done: true });
        if (!st.preventCancel) {
          const result = readableStreamCancel(r.stream, value);
          readableStreamDefaultReaderRelease(r);
          return react(result, () => ({ value, done: true }));
        }
        readableStreamDefaultReaderRelease(r);
        return resolved({ value, done: true });
      };
      st.ongoing = st.ongoing === undefined ? run() : then(st.ongoing, run, run);
      return st.ongoing;
    },
    writable: true,
    enumerable: true,
    configurable: true,
  });

  // ---- WritableStream (§5.2) ----

  class WritableStream {
    constructor(underlyingSink = undefined, strategy = undefined) {
      if (underlyingSink === null) throw new TypeError("underlyingSink can't be null");
      const sink = underlyingSink === undefined ? {} : underlyingSink;
      if (!isObject(sink)) throw new TypeError("underlyingSink must be an object");
      const strat = strategyDict(strategy);
      if (sink.type !== undefined) throw new RangeError("type must be undefined");
      const s = initializeWritableStream(this);
      const sizeAlgorithm = extractSizeAlgorithm(strat);
      const highWaterMark = extractHighWaterMark(strat, 1);
      setUpWritableStreamDefaultControllerFromUnderlyingSink(s, sink, highWaterMark, sizeAlgorithm);
    }
    get locked() {
      return isWritableStreamLocked(slot(WS, this, "WritableStream"));
    }
    abort(reason = undefined) {
      let s;
      try {
        s = slot(WS, this, "WritableStream");
      } catch (e) {
        return rejected(e);
      }
      if (isWritableStreamLocked(s)) return rejected(new TypeError("the stream is locked"));
      return writableStreamAbort(s, reason);
    }
    close() {
      let s;
      try {
        s = slot(WS, this, "WritableStream");
      } catch (e) {
        return rejected(e);
      }
      if (isWritableStreamLocked(s)) return rejected(new TypeError("the stream is locked"));
      if (writableStreamCloseQueuedOrInFlight(s)) return rejected(new TypeError("the stream is already closing"));
      return writableStreamClose(s);
    }
    getWriter() {
      return acquireWritableStreamDefaultWriter(slot(WS, this, "WritableStream"));
    }
  }

  function initializeWritableStream(stream) {
    const s = {
      object: stream,
      state: "writable",
      storedError: undefined,
      writer: undefined,
      controller: undefined,
      inFlightWriteRequest: undefined,
      closeRequest: undefined,
      inFlightCloseRequest: undefined,
      pendingAbortRequest: undefined,
      writeRequests: [],
      backpressure: false,
    };
    WS.set(stream, s);
    return s;
  }

  function createWritableStream(startAlgorithm, writeAlgorithm, closeAlgorithm, abortAlgorithm, highWaterMark = 1, sizeAlgorithm = () => 1) {
    const stream = Object.create(WritableStream.prototype);
    const s = initializeWritableStream(stream);
    const c = newWritableController();
    setUpWritableStreamDefaultController(s, c, startAlgorithm, writeAlgorithm, closeAlgorithm, abortAlgorithm, highWaterMark, sizeAlgorithm);
    return s;
  }

  function isWritableStreamLocked(s) {
    return s.writer !== undefined;
  }

  // §5.5.1 WritableStreamAbort.
  function writableStreamAbort(s, reason) {
    if (s.state === "closed" || s.state === "errored") return resolved(undefined);
    s.controller.abortController.abort(reason);
    const state = s.state;
    if (state === "closed" || state === "errored") return resolved(undefined);
    if (s.pendingAbortRequest !== undefined) return s.pendingAbortRequest.promise.promise;
    let wasAlreadyErroring = false;
    if (state === "erroring") {
      wasAlreadyErroring = true;
      reason = undefined;
    }
    const promise = newPromise();
    s.pendingAbortRequest = { promise, reason, wasAlreadyErroring };
    if (!wasAlreadyErroring) writableStreamStartErroring(s, reason);
    return promise.promise;
  }

  function writableStreamClose(s) {
    const state = s.state;
    if (state === "closed" || state === "errored") {
      return rejected(new TypeError("the stream is closed or errored"));
    }
    const promise = newPromise();
    s.closeRequest = promise;
    const w = s.writer;
    if (w !== undefined && s.backpressure && state === "writable") w.ready.resolve(undefined);
    writableStreamDefaultControllerClose(s.controller);
    return promise.promise;
  }

  function writableStreamAddWriteRequest(s) {
    const p = newPromise();
    s.writeRequests.push(p);
    return p.promise;
  }

  function writableStreamDealWithRejection(s, e) {
    if (s.state === "writable") {
      writableStreamStartErroring(s, e);
      return;
    }
    writableStreamFinishErroring(s);
  }

  function writableStreamStartErroring(s, reason) {
    const c = s.controller;
    s.state = "erroring";
    s.storedError = reason;
    const w = s.writer;
    if (w !== undefined) writableStreamDefaultWriterEnsureReadyPromiseRejected(w, reason);
    if (!writableStreamHasOperationMarkedInFlight(s) && c.started) writableStreamFinishErroring(s);
  }

  function writableStreamFinishErroring(s) {
    s.state = "errored";
    s.controller.errorSteps();
    const storedError = s.storedError;
    const requests = s.writeRequests;
    s.writeRequests = [];
    for (let i = 0; i < requests.length; i++) requests[i].reject(storedError);
    if (s.pendingAbortRequest === undefined) {
      writableStreamRejectCloseAndClosedPromiseIfNeeded(s);
      return;
    }
    const abortRequest = s.pendingAbortRequest;
    s.pendingAbortRequest = undefined;
    if (abortRequest.wasAlreadyErroring) {
      abortRequest.promise.reject(storedError);
      writableStreamRejectCloseAndClosedPromiseIfNeeded(s);
      return;
    }
    const p = s.controller.abortSteps(abortRequest.reason);
    react(
      p,
      () => {
        abortRequest.promise.resolve(undefined);
        writableStreamRejectCloseAndClosedPromiseIfNeeded(s);
      },
      (reason) => {
        abortRequest.promise.reject(reason);
        writableStreamRejectCloseAndClosedPromiseIfNeeded(s);
      }
    );
  }

  function writableStreamFinishInFlightWrite(s) {
    s.inFlightWriteRequest.resolve(undefined);
    s.inFlightWriteRequest = undefined;
  }

  function writableStreamFinishInFlightWriteWithError(s, e) {
    s.inFlightWriteRequest.reject(e);
    s.inFlightWriteRequest = undefined;
    writableStreamDealWithRejection(s, e);
  }

  function writableStreamFinishInFlightClose(s) {
    s.inFlightCloseRequest.resolve(undefined);
    s.inFlightCloseRequest = undefined;
    if (s.state === "erroring") {
      s.storedError = undefined;
      if (s.pendingAbortRequest !== undefined) {
        s.pendingAbortRequest.promise.resolve(undefined);
        s.pendingAbortRequest = undefined;
      }
    }
    s.state = "closed";
    const w = s.writer;
    if (w !== undefined) w.closed.resolve(undefined);
  }

  function writableStreamFinishInFlightCloseWithError(s, e) {
    s.inFlightCloseRequest.reject(e);
    s.inFlightCloseRequest = undefined;
    if (s.pendingAbortRequest !== undefined) {
      s.pendingAbortRequest.promise.reject(e);
      s.pendingAbortRequest = undefined;
    }
    writableStreamDealWithRejection(s, e);
  }

  function writableStreamCloseQueuedOrInFlight(s) {
    return s.closeRequest !== undefined || s.inFlightCloseRequest !== undefined;
  }

  function writableStreamHasOperationMarkedInFlight(s) {
    return s.inFlightWriteRequest !== undefined || s.inFlightCloseRequest !== undefined;
  }

  function writableStreamMarkCloseRequestInFlight(s) {
    s.inFlightCloseRequest = s.closeRequest;
    s.closeRequest = undefined;
  }

  function writableStreamMarkFirstWriteRequestInFlight(s) {
    s.inFlightWriteRequest = s.writeRequests.shift();
  }

  function writableStreamRejectCloseAndClosedPromiseIfNeeded(s) {
    if (s.closeRequest !== undefined) {
      s.closeRequest.reject(s.storedError);
      s.closeRequest = undefined;
    }
    const w = s.writer;
    if (w !== undefined) {
      w.closed.reject(s.storedError);
      ignore(w.closed.promise);
    }
  }

  function writableStreamUpdateBackpressure(s, backpressure) {
    const w = s.writer;
    if (w !== undefined && backpressure !== s.backpressure) {
      if (backpressure) w.ready = newPromise();
      else w.ready.resolve(undefined);
    }
    s.backpressure = backpressure;
  }

  // ---- WritableStreamDefaultWriter (§5.3) ----

  class WritableStreamDefaultWriter {
    constructor(stream) {
      setUpWritableStreamDefaultWriter(this, slot(WS, stream, "WritableStream"));
    }
    get closed() {
      return slot(WSDW, this, "WritableStreamDefaultWriter").closed.promise;
    }
    get desiredSize() {
      const w = slot(WSDW, this, "WritableStreamDefaultWriter");
      if (w.stream === undefined) throw new TypeError("the writer is released");
      return writableStreamDefaultWriterGetDesiredSize(w);
    }
    get ready() {
      return slot(WSDW, this, "WritableStreamDefaultWriter").ready.promise;
    }
    abort(reason = undefined) {
      let w;
      try {
        w = slot(WSDW, this, "WritableStreamDefaultWriter");
      } catch (e) {
        return rejected(e);
      }
      if (w.stream === undefined) return rejected(new TypeError("the writer is released"));
      return writableStreamAbort(w.stream, reason);
    }
    close() {
      let w;
      try {
        w = slot(WSDW, this, "WritableStreamDefaultWriter");
      } catch (e) {
        return rejected(e);
      }
      const s = w.stream;
      if (s === undefined) return rejected(new TypeError("the writer is released"));
      if (writableStreamCloseQueuedOrInFlight(s)) return rejected(new TypeError("the stream is already closing"));
      return writableStreamClose(s);
    }
    releaseLock() {
      const w = slot(WSDW, this, "WritableStreamDefaultWriter");
      if (w.stream === undefined) return;
      writableStreamDefaultWriterRelease(w);
    }
    write(chunk = undefined) {
      let w;
      try {
        w = slot(WSDW, this, "WritableStreamDefaultWriter");
      } catch (e) {
        return rejected(e);
      }
      if (w.stream === undefined) return rejected(new TypeError("the writer is released"));
      return writableStreamDefaultWriterWrite(w, chunk);
    }
  }

  function setUpWritableStreamDefaultWriter(writer, s) {
    if (isWritableStreamLocked(s)) throw new TypeError("the stream is locked");
    const w = { object: writer, stream: s, ready: newPromise(), closed: newPromise() };
    WSDW.set(writer, w);
    s.writer = w;
    const state = s.state;
    if (state === "writable") {
      if (!writableStreamCloseQueuedOrInFlight(s) && s.backpressure) {
        // ready stays pending
      } else w.ready.resolve(undefined);
    } else if (state === "erroring") {
      w.ready.reject(s.storedError);
      ignore(w.ready.promise);
    } else if (state === "closed") {
      w.ready.resolve(undefined);
      w.closed.resolve(undefined);
    } else {
      const e = s.storedError;
      w.ready.reject(e);
      ignore(w.ready.promise);
      w.closed.reject(e);
      ignore(w.closed.promise);
    }
    return w;
  }

  function acquireWritableStreamDefaultWriter(s) {
    const writer = Object.create(WritableStreamDefaultWriter.prototype);
    setUpWritableStreamDefaultWriter(writer, s);
    return writer;
  }

  // §5.5.5: reject a pending promise, or replace a settled one with a rejected one.
  function rejectOrReplace(d, e) {
    if (d.settled) d = newPromise();
    d.reject(e);
    ignore(d.promise);
    return d;
  }

  function writableStreamDefaultWriterEnsureClosedPromiseRejected(w, e) {
    w.closed = rejectOrReplace(w.closed, e);
  }

  function writableStreamDefaultWriterEnsureReadyPromiseRejected(w, e) {
    w.ready = rejectOrReplace(w.ready, e);
  }

  function writableStreamDefaultWriterGetDesiredSize(w) {
    const s = w.stream;
    const state = s.state;
    if (state === "errored" || state === "erroring") return null;
    if (state === "closed") return 0;
    return writableStreamDefaultControllerGetDesiredSize(s.controller);
  }

  function writableStreamDefaultWriterRelease(w) {
    const s = w.stream;
    const e = new TypeError("the writer was released");
    writableStreamDefaultWriterEnsureReadyPromiseRejected(w, e);
    writableStreamDefaultWriterEnsureClosedPromiseRejected(w, e);
    s.writer = undefined;
    w.stream = undefined;
  }

  function writableStreamDefaultWriterWrite(w, chunk) {
    const s = w.stream;
    const c = s.controller;
    const chunkSize = writableStreamDefaultControllerGetChunkSize(c, chunk);
    if (s !== w.stream) return rejected(new TypeError("the writer is released"));
    const state = s.state;
    if (state === "errored") return rejected(s.storedError);
    if (writableStreamCloseQueuedOrInFlight(s) || state === "closed") {
      return rejected(new TypeError("the stream is closing or closed"));
    }
    if (state === "erroring") return rejected(s.storedError);
    const p = writableStreamAddWriteRequest(s);
    writableStreamDefaultControllerWrite(c, chunk, chunkSize);
    return p;
  }

  // ---- WritableStreamDefaultController (§5.4) ----

  class WritableStreamDefaultController {
    constructor() {
      throw new TypeError("Illegal constructor");
    }
    get signal() {
      return slot(WSDC, this, "WritableStreamDefaultController").abortController.signal;
    }
    error(e = undefined) {
      const c = slot(WSDC, this, "WritableStreamDefaultController");
      if (c.stream.state !== "writable") return;
      writableStreamDefaultControllerError(c, e);
    }
  }

  const closeSentinel = {};

  function newWritableController() {
    const object = Object.create(WritableStreamDefaultController.prototype);
    const c = {
      object,
      stream: undefined,
      queue: [],
      queueTotalSize: 0,
      started: false,
      strategyHWM: 0,
      strategySizeAlgorithm: undefined,
      writeAlgorithm: undefined,
      closeAlgorithm: undefined,
      abortAlgorithm: undefined,
      abortController: new global.AbortController(),
    };
    c.abortSteps = (reason) => {
      const result = c.abortAlgorithm(reason);
      writableStreamDefaultControllerClearAlgorithms(c);
      return result;
    };
    c.errorSteps = () => resetQueue(c);
    WSDC.set(object, c);
    return c;
  }

  function setUpWritableStreamDefaultController(s, c, startAlgorithm, writeAlgorithm, closeAlgorithm, abortAlgorithm, highWaterMark, sizeAlgorithm) {
    c.stream = s;
    s.controller = c;
    resetQueue(c);
    c.started = false;
    c.strategySizeAlgorithm = sizeAlgorithm;
    c.strategyHWM = highWaterMark;
    c.writeAlgorithm = writeAlgorithm;
    c.closeAlgorithm = closeAlgorithm;
    c.abortAlgorithm = abortAlgorithm;
    writableStreamUpdateBackpressure(s, writableStreamDefaultControllerGetBackpressure(c));
    const startResult = startAlgorithm();
    react(
      resolved(startResult),
      () => {
        c.started = true;
        writableStreamDefaultControllerAdvanceQueueIfNeeded(c);
      },
      (r) => {
        c.started = true;
        writableStreamDealWithRejection(s, r);
      }
    );
  }

  function setUpWritableStreamDefaultControllerFromUnderlyingSink(s, sink, highWaterMark, sizeAlgorithm) {
    const c = newWritableController();
    const start = sink.start;
    const write = sink.write;
    const close = sink.close;
    const abort = sink.abort;
    for (const [name, f] of [["start", start], ["write", write], ["close", close], ["abort", abort]]) {
      if (f !== undefined && typeof f !== "function") throw new TypeError(name + " must be a function");
    }
    const startAlgorithm = start === undefined ? () => undefined : () => call(start, sink, [c.object]);
    const promised = (f) => {
      try {
        return resolved(f());
      } catch (e) {
        return rejected(e);
      }
    };
    const writeAlgorithm = write === undefined ? () => resolved(undefined) : (chunk) => promised(() => call(write, sink, [chunk, c.object]));
    const closeAlgorithm = close === undefined ? () => resolved(undefined) : () => promised(() => call(close, sink, []));
    const abortAlgorithm = abort === undefined ? () => resolved(undefined) : (reason) => promised(() => call(abort, sink, [reason]));
    setUpWritableStreamDefaultController(s, c, startAlgorithm, writeAlgorithm, closeAlgorithm, abortAlgorithm, highWaterMark, sizeAlgorithm);
  }

  function writableStreamDefaultControllerAdvanceQueueIfNeeded(c) {
    const s = c.stream;
    if (!c.started) return;
    if (s.inFlightWriteRequest !== undefined) return;
    const state = s.state;
    if (state === "erroring") {
      writableStreamFinishErroring(s);
      return;
    }
    if (c.queue.length === 0) return;
    const value = peekQueueValue(c);
    if (value === closeSentinel) writableStreamDefaultControllerProcessClose(c);
    else writableStreamDefaultControllerProcessWrite(c, value);
  }

  function writableStreamDefaultControllerClearAlgorithms(c) {
    c.writeAlgorithm = undefined;
    c.closeAlgorithm = undefined;
    c.abortAlgorithm = undefined;
    c.strategySizeAlgorithm = undefined;
  }

  function writableStreamDefaultControllerClose(c) {
    enqueueValueWithSize(c, closeSentinel, 0);
    writableStreamDefaultControllerAdvanceQueueIfNeeded(c);
  }

  function writableStreamDefaultControllerError(c, e) {
    const s = c.stream;
    writableStreamDefaultControllerClearAlgorithms(c);
    writableStreamStartErroring(s, e);
  }

  function writableStreamDefaultControllerErrorIfNeeded(c, e) {
    if (c.stream.state === "writable") writableStreamDefaultControllerError(c, e);
  }

  function writableStreamDefaultControllerGetBackpressure(c) {
    return writableStreamDefaultControllerGetDesiredSize(c) <= 0;
  }

  function writableStreamDefaultControllerGetChunkSize(c, chunk) {
    if (c.strategySizeAlgorithm === undefined) return 1;
    try {
      return c.strategySizeAlgorithm(chunk);
    } catch (e) {
      writableStreamDefaultControllerErrorIfNeeded(c, e);
      return 1;
    }
  }

  function writableStreamDefaultControllerGetDesiredSize(c) {
    return c.strategyHWM - c.queueTotalSize;
  }

  function writableStreamDefaultControllerProcessClose(c) {
    const s = c.stream;
    writableStreamMarkCloseRequestInFlight(s);
    dequeueValue(c);
    const p = c.closeAlgorithm();
    writableStreamDefaultControllerClearAlgorithms(c);
    react(
      p,
      () => writableStreamFinishInFlightClose(s),
      (r) => writableStreamFinishInFlightCloseWithError(s, r)
    );
  }

  function writableStreamDefaultControllerProcessWrite(c, chunk) {
    const s = c.stream;
    writableStreamMarkFirstWriteRequestInFlight(s);
    const p = c.writeAlgorithm(chunk);
    react(
      p,
      () => {
        writableStreamFinishInFlightWrite(s);
        const state = s.state;
        dequeueValue(c);
        if (!writableStreamCloseQueuedOrInFlight(s) && state === "writable") {
          writableStreamUpdateBackpressure(s, writableStreamDefaultControllerGetBackpressure(c));
        }
        writableStreamDefaultControllerAdvanceQueueIfNeeded(c);
      },
      (reason) => {
        if (s.state === "writable") writableStreamDefaultControllerClearAlgorithms(c);
        writableStreamFinishInFlightWriteWithError(s, reason);
      }
    );
  }

  function writableStreamDefaultControllerWrite(c, chunk, chunkSize) {
    try {
      enqueueValueWithSize(c, chunk, chunkSize);
    } catch (e) {
      writableStreamDefaultControllerErrorIfNeeded(c, e);
      return;
    }
    const s = c.stream;
    if (!writableStreamCloseQueuedOrInFlight(s) && s.state === "writable") {
      writableStreamUpdateBackpressure(s, writableStreamDefaultControllerGetBackpressure(c));
    }
    writableStreamDefaultControllerAdvanceQueueIfNeeded(c);
  }

  // ---- piping (§4.9.1 ReadableStreamPipeTo) ----

  function readableStreamPipeTo(source, dest, preventClose, preventAbort, preventCancel, signal) {
    const reader = acquireReadableStreamDefaultReader(source);
    const r = RSDR.get(reader);
    const writer = acquireWritableStreamDefaultWriter(dest);
    const w = WSDW.get(writer);
    source.disturbed = true;
    let shuttingDown = false;
    const promise = newPromise();
    let currentWrite = resolved(undefined);
    let abortAlgorithm;

    function finalize(isError, error) {
      writableStreamDefaultWriterRelease(w);
      readableStreamDefaultReaderRelease(r);
      if (signal !== undefined) signal.removeEventListener("abort", abortAlgorithm);
      if (isError) promise.reject(error);
      else promise.resolve(undefined);
    }
    function waitForWrites(next) {
      const old = currentWrite;
      react(old, () => (old === currentWrite ? next() : waitForWrites(next)), () => (old === currentWrite ? next() : waitForWrites(next)));
    }
    function shutdownWithAction(action, isError, error) {
      if (shuttingDown) return;
      shuttingDown = true;
      const go = () => {
        react(
          action(),
          () => finalize(isError, error),
          (e) => finalize(true, e)
        );
      };
      if (dest.state === "writable" && !writableStreamCloseQueuedOrInFlight(dest)) waitForWrites(go);
      else go();
    }
    function shutdown(isError, error) {
      if (shuttingDown) return;
      shuttingDown = true;
      if (dest.state === "writable" && !writableStreamCloseQueuedOrInFlight(dest)) {
        waitForWrites(() => finalize(isError, error));
      } else finalize(isError, error);
    }

    if (signal !== undefined) {
      abortAlgorithm = () => {
        const error = signal.reason;
        const actions = [];
        if (!preventAbort) {
          actions.push(() => (dest.state === "writable" ? writableStreamAbort(dest, error) : resolved(undefined)));
        }
        if (!preventCancel) {
          actions.push(() => (source.state === "readable" ? readableStreamCancel(source, error) : resolved(undefined)));
        }
        shutdownWithAction(
          () => {
            const ps = [];
            for (let i = 0; i < actions.length; i++) ps.push(actions[i]());
            return Promise.all(ps);
          },
          true,
          error
        );
      };
      if (signal.aborted) {
        abortAlgorithm();
        return promise.promise;
      }
      signal.addEventListener("abort", abortAlgorithm);
    }

    // Errors and closing must be propagated forward and backward.
    function checkStates() {
      if (source.state === "errored") {
        const e = source.storedError;
        if (!preventAbort) shutdownWithAction(() => writableStreamAbort(dest, e), true, e);
        else shutdown(true, e);
        return true;
      }
      if (dest.state === "errored" || dest.state === "erroring") {
        const e = dest.storedError;
        if (!preventCancel) shutdownWithAction(() => readableStreamCancel(source, e), true, e);
        else shutdown(true, e);
        return true;
      }
      if (source.state === "closed") {
        if (!preventClose) {
          shutdownWithAction(() => {
            if (writableStreamCloseQueuedOrInFlight(dest) || dest.state === "closed") return resolved(undefined);
            return writableStreamDefaultWriterCloseWithErrorPropagation(w);
          }, false);
        } else shutdown(false);
        return true;
      }
      if (writableStreamCloseQueuedOrInFlight(dest) || dest.state === "closed") {
        const e = new TypeError("the destination closed");
        if (!preventCancel) shutdownWithAction(() => readableStreamCancel(source, e), true, e);
        else shutdown(true, e);
        return true;
      }
      return false;
    }

    react(r.closed.promise, () => {
      if (!shuttingDown) checkStates();
    }, () => {
      if (!shuttingDown) checkStates();
    });
    react(w.closed.promise, undefined, () => {
      if (!shuttingDown) checkStates();
    });

    function pipeStep() {
      if (shuttingDown) return;
      if (checkStates()) return;
      react(w.ready.promise, () => {
        if (shuttingDown || checkStates()) return;
        readableStreamDefaultReaderRead(r, {
          chunk: (chunk) => {
            currentWrite = writableStreamDefaultWriterWrite(w, chunk);
            ignore(currentWrite);
            queueMicrotask(pipeStep);
          },
          close: () => {
            if (!shuttingDown) checkStates();
          },
          error: () => {
            if (!shuttingDown) checkStates();
          },
        });
      }, () => {
        if (!shuttingDown) checkStates();
      });
    }
    pipeStep();
    return promise.promise;
  }

  function writableStreamDefaultWriterCloseWithErrorPropagation(w) {
    const s = w.stream;
    const state = s.state;
    if (writableStreamCloseQueuedOrInFlight(s) || state === "closed") return resolved(undefined);
    if (state === "errored") return rejected(s.storedError);
    return writableStreamClose(s);
  }

  // ---- TransformStream (§6.2) ----

  class TransformStream {
    constructor(transformer = undefined, writableStrategy = undefined, readableStrategy = undefined) {
      if (transformer === null) throw new TypeError("transformer can't be null");
      const t = transformer === undefined ? {} : transformer;
      if (!isObject(t)) throw new TypeError("transformer must be an object");
      const ws = strategyDict(writableStrategy);
      const rs = strategyDict(readableStrategy);
      if (t.readableType !== undefined) throw new RangeError("readableType must be undefined");
      if (t.writableType !== undefined) throw new RangeError("writableType must be undefined");
      const readableHWM = extractHighWaterMark(rs, 0);
      const readableSize = extractSizeAlgorithm(rs);
      const writableHWM = extractHighWaterMark(ws, 1);
      const writableSize = extractSizeAlgorithm(ws);
      const start = newPromise();
      const s = initializeTransformStream(this, start.promise, writableHWM, writableSize, readableHWM, readableSize);
      setUpTransformStreamDefaultControllerFromTransformer(s, t);
      const startMethod = t.start;
      if (startMethod !== undefined && typeof startMethod !== "function") throw new TypeError("start must be a function");
      if (startMethod !== undefined) start.resolve(call(startMethod, t, [s.controller.object]));
      else start.resolve(undefined);
    }
    get readable() {
      return slot(TS, this, "TransformStream").readable.object;
    }
    get writable() {
      return slot(TS, this, "TransformStream").writable.object;
    }
  }

  function initializeTransformStream(stream, startPromise, writableHWM, writableSize, readableHWM, readableSize) {
    const s = { object: stream, readable: undefined, writable: undefined, backpressure: undefined, backpressureChange: undefined, controller: undefined };
    TS.set(stream, s);
    const startAlgorithm = () => startPromise;
    s.writable = createWritableStream(
      startAlgorithm,
      (chunk) => transformStreamDefaultSinkWriteAlgorithm(s, chunk),
      () => transformStreamDefaultSinkCloseAlgorithm(s),
      (reason) => transformStreamDefaultSinkAbortAlgorithm(s, reason),
      writableHWM,
      writableSize
    );
    s.readable = createReadableStream(
      startAlgorithm,
      () => transformStreamDefaultSourcePullAlgorithm(s),
      (reason) => transformStreamDefaultSourceCancelAlgorithm(s, reason),
      readableHWM,
      readableSize
    );
    s.backpressure = undefined;
    s.backpressureChange = undefined;
    transformStreamSetBackpressure(s, true);
    return s;
  }

  function transformStreamError(s, e) {
    readableStreamDefaultControllerError(s.readable.controller, e);
    transformStreamErrorWritableAndUnblockWrite(s, e);
  }

  function transformStreamErrorWritableAndUnblockWrite(s, e) {
    transformStreamDefaultControllerClearAlgorithms(s.controller);
    writableStreamDefaultControllerErrorIfNeeded(s.writable.controller, e);
    transformStreamUnblockWrite(s);
  }

  function transformStreamUnblockWrite(s) {
    if (s.backpressure) transformStreamSetBackpressure(s, false);
  }

  function transformStreamSetBackpressure(s, backpressure) {
    if (s.backpressureChange !== undefined) s.backpressureChange.resolve(undefined);
    s.backpressureChange = newPromise();
    s.backpressure = backpressure;
  }

  class TransformStreamDefaultController {
    constructor() {
      throw new TypeError("Illegal constructor");
    }
    get desiredSize() {
      const c = slot(TSDC, this, "TransformStreamDefaultController");
      return readableStreamDefaultControllerGetDesiredSize(c.stream.readable.controller);
    }
    enqueue(chunk = undefined) {
      transformStreamDefaultControllerEnqueue(slot(TSDC, this, "TransformStreamDefaultController"), chunk);
    }
    error(reason = undefined) {
      transformStreamError(slot(TSDC, this, "TransformStreamDefaultController").stream, reason);
    }
    terminate() {
      transformStreamDefaultControllerTerminate(slot(TSDC, this, "TransformStreamDefaultController"));
    }
  }

  function setUpTransformStreamDefaultControllerFromTransformer(s, transformer) {
    const object = Object.create(TransformStreamDefaultController.prototype);
    const c = { object, stream: s, transformAlgorithm: undefined, flushAlgorithm: undefined, cancelAlgorithm: undefined, finishPromise: undefined };
    TSDC.set(object, c);
    s.controller = c;
    const transform = transformer.transform;
    const flush = transformer.flush;
    const cancel = transformer.cancel;
    for (const [name, f] of [["transform", transform], ["flush", flush], ["cancel", cancel]]) {
      if (f !== undefined && typeof f !== "function") throw new TypeError(name + " must be a function");
    }
    const promised = (f) => {
      try {
        return resolved(f());
      } catch (e) {
        return rejected(e);
      }
    };
    c.transformAlgorithm =
      transform === undefined
        ? (chunk) => {
            try {
              transformStreamDefaultControllerEnqueue(c, chunk);
              return resolved(undefined);
            } catch (e) {
              return rejected(e);
            }
          }
        : (chunk) => promised(() => call(transform, transformer, [chunk, object]));
    c.flushAlgorithm = flush === undefined ? () => resolved(undefined) : () => promised(() => call(flush, transformer, [object]));
    c.cancelAlgorithm = cancel === undefined ? () => resolved(undefined) : (reason) => promised(() => call(cancel, transformer, [reason]));
  }

  function transformStreamDefaultControllerClearAlgorithms(c) {
    c.transformAlgorithm = undefined;
    c.flushAlgorithm = undefined;
    c.cancelAlgorithm = undefined;
  }

  function transformStreamDefaultControllerEnqueue(c, chunk) {
    const s = c.stream;
    const rc = s.readable.controller;
    if (!readableStreamDefaultControllerCanCloseOrEnqueue(rc)) throw new TypeError("the readable side can't be enqueued to");
    try {
      readableStreamDefaultControllerEnqueue(rc, chunk);
    } catch (e) {
      transformStreamErrorWritableAndUnblockWrite(s, e);
      throw s.readable.storedError;
    }
    const backpressure = readableStreamDefaultControllerHasBackpressure(rc);
    if (backpressure !== s.backpressure) transformStreamSetBackpressure(s, true);
  }

  function transformStreamDefaultControllerPerformTransform(c, chunk) {
    return react(c.transformAlgorithm(chunk), undefined, (r) => {
      transformStreamError(c.stream, r);
      throw r;
    });
  }

  function transformStreamDefaultControllerTerminate(c) {
    const s = c.stream;
    readableStreamDefaultControllerClose(s.readable.controller);
    transformStreamErrorWritableAndUnblockWrite(s, new TypeError("the transform stream was terminated"));
  }

  function transformStreamDefaultSinkWriteAlgorithm(s, chunk) {
    const c = s.controller;
    if (s.backpressure) {
      return react(s.backpressureChange.promise, () => {
        const w = s.writable;
        if (w.state === "erroring") throw w.storedError;
        return transformStreamDefaultControllerPerformTransform(c, chunk);
      });
    }
    return transformStreamDefaultControllerPerformTransform(c, chunk);
  }

  function transformStreamDefaultSinkAbortAlgorithm(s, reason) {
    const c = s.controller;
    if (c.finishPromise !== undefined) return c.finishPromise.promise;
    const readable = s.readable;
    c.finishPromise = newPromise();
    const p = c.cancelAlgorithm(reason);
    transformStreamDefaultControllerClearAlgorithms(c);
    react(
      p,
      () => {
        if (readable.state === "errored") c.finishPromise.reject(readable.storedError);
        else {
          readableStreamDefaultControllerError(readable.controller, reason);
          c.finishPromise.resolve(undefined);
        }
      },
      (r) => {
        readableStreamDefaultControllerError(readable.controller, r);
        c.finishPromise.reject(r);
      }
    );
    return c.finishPromise.promise;
  }

  function transformStreamDefaultSinkCloseAlgorithm(s) {
    const c = s.controller;
    if (c.finishPromise !== undefined) return c.finishPromise.promise;
    const readable = s.readable;
    c.finishPromise = newPromise();
    const p = c.flushAlgorithm();
    transformStreamDefaultControllerClearAlgorithms(c);
    react(
      p,
      () => {
        if (readable.state === "errored") c.finishPromise.reject(readable.storedError);
        else {
          readableStreamDefaultControllerClose(readable.controller);
          c.finishPromise.resolve(undefined);
        }
      },
      (r) => {
        readableStreamDefaultControllerError(readable.controller, r);
        c.finishPromise.reject(r);
      }
    );
    return c.finishPromise.promise;
  }

  function transformStreamDefaultSourcePullAlgorithm(s) {
    transformStreamSetBackpressure(s, false);
    return s.backpressureChange.promise;
  }

  function transformStreamDefaultSourceCancelAlgorithm(s, reason) {
    const c = s.controller;
    if (c.finishPromise !== undefined) return c.finishPromise.promise;
    const writable = s.writable;
    c.finishPromise = newPromise();
    const p = c.cancelAlgorithm(reason);
    transformStreamDefaultControllerClearAlgorithms(c);
    react(
      p,
      () => {
        if (writable.state === "errored") c.finishPromise.reject(writable.storedError);
        else {
          writableStreamDefaultControllerErrorIfNeeded(writable.controller, reason);
          transformStreamUnblockWrite(s);
          c.finishPromise.resolve(undefined);
        }
      },
      (r) => {
        writableStreamDefaultControllerErrorIfNeeded(writable.controller, r);
        transformStreamUnblockWrite(s);
        c.finishPromise.reject(r);
      }
    );
    return c.finishPromise.promise;
  }

  // ---- TextEncoderStream / TextDecoderStream (Encoding §5.2, §5.4) ----

  class TextEncoderStream {
    #ts;
    #encoder;
    constructor() {
      const encoder = new global.TextEncoder();
      let pending = "";
      this.#encoder = encoder;
      this.#ts = new TransformStream({
        transform(chunk, controller) {
          let s = pending + `${chunk}`;
          pending = "";
          const last = s.length ? s.charCodeAt(s.length - 1) : 0;
          if (last >= 0xd800 && last <= 0xdbff) {
            pending = s[s.length - 1];
            s = s.slice(0, -1);
          }
          if (s.length) controller.enqueue(encoder.encode(s));
        },
        flush(controller) {
          if (pending) controller.enqueue(encoder.encode(pending));
        },
      });
    }
    get encoding() {
      return this.#encoder.encoding;
    }
    get readable() {
      return this.#ts.readable;
    }
    get writable() {
      return this.#ts.writable;
    }
  }

  class TextDecoderStream {
    #ts;
    #decoder;
    constructor(label = "utf-8", options = undefined) {
      const decoder = new global.TextDecoder(label, options);
      this.#decoder = decoder;
      this.#ts = new TransformStream({
        transform(chunk, controller) {
          const s = decoder.decode(chunk, { stream: true });
          if (s) controller.enqueue(s);
        },
        flush(controller) {
          const s = decoder.decode();
          if (s) controller.enqueue(s);
        },
      });
    }
    get encoding() {
      return this.#decoder.encoding;
    }
    get fatal() {
      return this.#decoder.fatal;
    }
    get ignoreBOM() {
      return this.#decoder.ignoreBOM;
    }
    get readable() {
      return this.#ts.readable;
    }
    get writable() {
      return this.#ts.writable;
    }
  }

  // ---- WebIDL-shaped members and the hooks the host uses ----

  const classes = {
    ReadableStream,
    ReadableStreamDefaultReader,
    ReadableStreamDefaultController,
    WritableStream,
    WritableStreamDefaultWriter,
    WritableStreamDefaultController,
    TransformStream,
    TransformStreamDefaultController,
    ByteLengthQueuingStrategy,
    CountQueuingStrategy,
    TextEncoderStream,
    TextDecoderStream,
  };
  const names = ObjectGetOwnPropertyNames(classes);
  for (let i = 0; i < names.length; i++) {
    const name = names[i];
    const proto = classes[name].prototype;
    // Operations and attributes are enumerable (WebIDL §3.7.4–3.7.6).
    const keys = ObjectGetOwnPropertyNames(proto);
    for (let k = 0; k < keys.length; k++) {
      if (keys[k] === "constructor") continue;
      const d = ObjectGetOwnPropertyDescriptor(proto, keys[k]);
      d.enumerable = true;
      ObjectDefineProperty(proto, keys[k], d);
    }
    ObjectDefineProperty(proto, Symbol_.toStringTag, { value: name, configurable: true });
  }
  ObjectDefineProperty(ReadableStream, "from", { enumerable: true });
  ObjectDefineProperty(ReadableStream.prototype, Symbol_.asyncIterator, {
    value: ReadableStream.prototype.values,
    writable: true,
    configurable: true,
  });
  ObjectDefineProperty(ReadableStreamAsyncIteratorPrototype, Symbol_.toStringTag, {
    value: "ReadableStream AsyncIterator",
    configurable: true,
  });

  // A stream of the given Uint8Array chunks (bodies, Blob.stream()).
  function fromChunks(chunks) {
    let i = 0;
    const s = createReadableStream(
      () => undefined,
      () => {
        if (i < chunks.length) readableStreamDefaultControllerEnqueue(s.controller, chunks[i++]);
        else readableStreamDefaultControllerClose(s.controller);
        return resolved(undefined);
      },
      () => resolved(undefined),
      0
    );
    return s.object;
  }
  // Whether a stream is locked or disturbed (a body that can't be read again).
  function unusable(stream) {
    const s = RS.get(stream);
    return s === undefined ? false : s.disturbed || isReadableStreamLocked(s);
  }
  // Read a whole stream of Uint8Array chunks into one (a body consumed after it streamed).
  function readAll(stream) {
    const reader = acquireReadableStreamDefaultReader(RS.get(stream));
    const parts = [];
    let total = 0;
    const p = newPromise();
    const step = () => {
      react(reader.read(), (r) => {
        if (r.done) {
          const out = new Uint8Array_(total);
          let at = 0;
          for (let i = 0; i < parts.length; i++) {
            out.set(parts[i], at);
            at += parts[i].byteLength;
          }
          p.resolve(out);
          return;
        }
        const c = r.value;
        if (!(c instanceof Uint8Array_)) {
          p.reject(new TypeError("a body stream chunk isn't a Uint8Array"));
          return;
        }
        parts.push(c);
        total += c.byteLength;
        step();
      }, p.reject);
    };
    step();
    return p.promise;
  }
  function disturbed(stream) {
    const s = RS.get(stream);
    return s !== undefined && s.disturbed;
  }
  return { classes, fromChunks, unusable, disturbed, readAll, ArrayBuffer: ArrayBuffer_, internal };
});
