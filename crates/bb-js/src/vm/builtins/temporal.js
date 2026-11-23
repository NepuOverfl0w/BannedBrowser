"use strict";
// Temporal (ECMA-262 2026 draft §Temporal, from the Temporal proposal), self-hosted: see
// selfhost.rs for the rules this code follows (no patchable built-ins; `I` holds natives).
// The ISO 8601 calendar is the only calendar; time zones are "UTC" and UTC offsets.
(function (I) {
  const G = I.global;
  const TypeError = I.TypeError;
  const RangeError = I.RangeError;
  const MathO = G.Math;
  const floor = MathO.floor;
  const trunc = MathO.trunc;
  const abs = MathO.abs;
  const NumberF = G.Number;
  const BigIntF = G.BigInt;
  const call = I.call;
  const sub = I.substring;
  const cu = I.codeUnit;
  const ObjectProto = I.ObjectPrototype;

  const isObject = (v) => (typeof v === "object" && v !== null) || typeof v === "function";
  const isFiniteN = (x) => typeof x === "number" && x === x && x !== Infinity && x !== -Infinity;
  const mod = (a, n) => {
    const r = a % n;
    return r < 0 ? r + n : r + 0;
  };
  const bmod = (a, n) => {
    const r = a % n;
    return r < 0n ? r + n : r;
  };
  // Floor division of BigInts.
  const bdiv = (a, n) => {
    const q = a / n;
    return a % n !== 0n && (a < 0n) !== (n < 0n) ? q - 1n : q;
  };
  const babs = (a) => (a < 0n ? -a : a);
  const bsign = (a) => (a < 0n ? -1 : a > 0n ? 1 : 0);

  // ---- brands and slots ----

  const DATE = 1, TIME = 2, DATETIME = 3, YEARMONTH = 4, MONTHDAY = 5, DURATION = 6, INSTANT = 7, ZONED = 8;
  const NAMES = ["", "Temporal.PlainDate", "Temporal.PlainTime", "Temporal.PlainDateTime",
    "Temporal.PlainYearMonth", "Temporal.PlainMonthDay", "Temporal.Duration", "Temporal.Instant",
    "Temporal.ZonedDateTime"];
  const slots = (o, b) => I.slots(o, b, NAMES[b]);
  const brand = (o) => I.brand(o);
  const protos = [];

  // An object of `brand` with prototype from `newTarget` (or the intrinsic).
  function create(b, values, proto) {
    const o = I.objectCreate(proto === undefined ? protos[b] : proto);
    I.setSlots(o, b, values);
    return o;
  }

  // GetPrototypeFromConstructor for a constructor called with `new`.
  function protoFrom(nt, b) {
    const p = nt.prototype;
    return isObject(p) ? p : protos[b];
  }

  // ---- conversions ----

  // ToIntegerWithTruncation.
  function toIntegerT(v) {
    const n = +v;
    if (!isFiniteN(n)) throw new RangeError("the value must be a finite number");
    return trunc(n) + 0;
  }

  // ToIntegerIfIntegral.
  function toIntegerIfIntegral(v) {
    const n = +v;
    if (!isFiniteN(n)) throw new RangeError("the value must be a finite number");
    if (trunc(n) !== n) throw new RangeError("the value must be an integer");
    return n + 0;
  }

  // ToPositiveIntegerWithTruncation.
  function toPositiveIntegerT(v) {
    const n = toIntegerT(v);
    if (n <= 0) throw new RangeError("the value must be positive");
    return n;
  }

  const toStr = (v) => `${v}`;

  // ToPrimitiveAndRequireString.
  function toPrimitiveString(v) {
    let p = v;
    if (isObject(v)) p = I.toPrimitiveValue(v, "string");
    if (typeof p !== "string") throw new TypeError("the value must be a string");
    return p;
  }

  // ---- options (§13 GetOptionsObject, GetOption) ----

  function getOptionsObject(options) {
    if (options === undefined) return I.objectCreate(null);
    if (isObject(options)) return options;
    throw new TypeError("options must be an object");
  }

  function getStringOption(options, key, values, fallback) {
    const v = options[key];
    if (v === undefined) return fallback;
    const s = toStr(v);
    if (values !== undefined && !includes(values, s)) throw new RangeError(`${s} is not a valid value for ${key}`);
    return s;
  }

  function includes(list, x) {
    for (let i = 0; i < list.length; i++) if (list[i] === x) return true;
    return false;
  }

  const overflowOpt = (o) => getStringOption(o, "overflow", ["constrain", "reject"], "constrain");
  const disambiguationOpt = (o) =>
    getStringOption(o, "disambiguation", ["compatible", "earlier", "later", "reject"], "compatible");
  const offsetOpt = (o, fallback) => getStringOption(o, "offset", ["prefer", "use", "ignore", "reject"], fallback);
  const calendarNameOpt = (o) => getStringOption(o, "calendarName", ["auto", "always", "never", "critical"], "auto");
  const timeZoneNameOpt = (o) => getStringOption(o, "timeZoneName", ["auto", "never", "critical"], "auto");
  const showOffsetOpt = (o) => getStringOption(o, "offset", ["auto", "never"], "auto");
  const MODES = ["ceil", "floor", "expand", "trunc", "halfCeil", "halfFloor", "halfExpand", "halfTrunc", "halfEven"];
  const roundingModeOpt = (o, fallback) => getStringOption(o, "roundingMode", MODES, fallback);

  function roundingIncrementOpt(o) {
    const v = o.roundingIncrement;
    if (v === undefined) return 1;
    const n = toIntegerT(v);
    if (n < 1 || n > 1e9) throw new RangeError("roundingIncrement is out of range");
    return n;
  }

  // GetTemporalFractionalSecondDigitsOption: "auto" or 0–9.
  function fractionalDigitsOpt(o) {
    const v = o.fractionalSecondDigits;
    if (v === undefined) return "auto";
    if (typeof v !== "number") {
      if (toStr(v) !== "auto") throw new RangeError("fractionalSecondDigits must be 'auto' or 0–9");
      return "auto";
    }
    if (!isFiniteN(v)) throw new RangeError("fractionalSecondDigits must be 'auto' or 0–9");
    const d = floor(v);
    if (d < 0 || d > 9) throw new RangeError("fractionalSecondDigits must be 'auto' or 0–9");
    return d;
  }

  // ---- units ----

  const UNITS = ["year", "month", "week", "day", "hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
  const PLURAL = ["years", "months", "weeks", "days", "hours", "minutes", "seconds", "milliseconds", "microseconds",
    "nanoseconds"];
  const unitIndex = (u) => {
    for (let i = 0; i < 10; i++) if (UNITS[i] === u) return i;
    return -1;
  };
  // larger: lower index
  const largerUnit = (a, b) => (unitIndex(a) <= unitIndex(b) ? a : b);
  const isDateUnit = (u) => unitIndex(u) <= 3;
  const NS = [0n, 0n, 0n, 86400000000000n, 3600000000000n, 60000000000n, 1000000000n, 1000000n, 1000n, 1n];
  const unitNs = (u) => NS[unitIndex(u)];

  // GetTemporalUnitValuedOption(options, key, unitGroup, default, extra).
  // group: "date" | "time" | "datetime"; fallback "required" makes undefined a RangeError.
  function unitOpt(o, key, group, fallback, extra) {
    let v = o[key];
    if (v === undefined) {
      if (fallback === "required") throw new RangeError(`${key} is required`);
      return fallback;
    }
    v = toStr(v);
    for (let i = 0; i < 10; i++) if (PLURAL[i] === v) v = UNITS[i];
    if (extra !== undefined && includes(extra, v)) return v;
    const i = unitIndex(v);
    if (i < 0) throw new RangeError(`${v} is not a valid value for ${key}`);
    if (group === "date" && i > 3) throw new RangeError(`${v} is not a date unit`);
    if (group === "time" && i < 4) throw new RangeError(`${v} is not a time unit`);
    return v;
  }

  // ValidateTemporalRoundingIncrement.
  function validateIncrement(inc, dividend, inclusive) {
    const maximum = inclusive ? dividend : dividend - 1;
    if (inc > maximum) throw new RangeError("roundingIncrement is out of range");
    if (dividend % inc !== 0) throw new RangeError("roundingIncrement must divide the unit evenly");
  }

  const MAXIMUM = { hour: 24, minute: 60, second: 60, millisecond: 1000, microsecond: 1000, nanosecond: 1000 };

  // ---- rounding ----

  function unsignedMode(mode, negative) {
    switch (mode) {
      case "ceil": return negative ? "zero" : "infinity";
      case "floor": return negative ? "infinity" : "zero";
      case "expand": return "infinity";
      case "trunc": return "zero";
      case "halfCeil": return negative ? "half-zero" : "half-infinity";
      case "halfFloor": return negative ? "half-infinity" : "half-zero";
      case "halfExpand": return "half-infinity";
      case "halfTrunc": return "half-zero";
      default: return "half-even";
    }
  }

  // RoundNumberToIncrement on exact integers (BigInt): a multiple of `inc`.
  function roundBig(x, inc, mode) {
    const neg = x < 0n;
    const a = neg ? -x : x;
    let q = a / inc;
    const r = a % inc;
    if (r !== 0n) {
      const um = unsignedMode(mode, neg);
      if (um === "infinity") q += 1n;
      else if (um !== "zero") {
        const twice = r * 2n;
        if (twice > inc) q += 1n;
        else if (twice === inc) {
          if (um === "half-infinity" || (um === "half-even" && q % 2n === 1n)) q += 1n;
        }
      }
    }
    const res = q * inc;
    return neg ? -res : res;
  }

  // ---- ISO calendar arithmetic ----

  const isLeap = (y) => mod(y, 4) === 0 && (mod(y, 100) !== 0 || mod(y, 400) === 0);
  const daysInMonth = (y, m) => (m === 2 ? (isLeap(y) ? 29 : 28) : m === 4 || m === 6 || m === 9 || m === 11 ? 30 : 31);
  const daysInYear = (y) => (isLeap(y) ? 366 : 365);

  // Days from 1970-01-01 (proleptic Gregorian).
  function epochDays(y, m, d) {
    const yy = m <= 2 ? y - 1 : y;
    const era = floor(yy / 400);
    const yoe = yy - era * 400;
    const mp = m > 2 ? m - 3 : m + 9;
    const doy = floor((153 * mp + 2) / 5) + d - 1;
    const doe = yoe * 365 + floor(yoe / 4) - floor(yoe / 100) + doy;
    return era * 146097 + doe - 719468;
  }

  function fromEpochDays(days) {
    const z = days + 719468;
    const era = floor(z / 146097);
    const doe = z - era * 146097;
    const yoe = floor((doe - floor(doe / 1460) + floor(doe / 36524) - floor(doe / 146096)) / 365);
    const doy = doe - (365 * yoe + floor(yoe / 4) - floor(yoe / 100));
    const mp = floor((5 * doy + 2) / 153);
    const d = doy - floor((153 * mp + 2) / 5) + 1;
    const m = mp < 10 ? mp + 3 : mp - 9;
    return { year: yoe + era * 400 + (m <= 2 ? 1 : 0), month: m, day: d };
  }

  const isValidISODate = (y, m, d) => m >= 1 && m <= 12 && d >= 1 && d <= daysInMonth(y, m);

  // RegulateISODate.
  function regulateDate(y, m, d, overflow) {
    if (overflow === "reject") {
      if (!isValidISODate(y, m, d)) throw new RangeError("the date is out of range");
      return { year: y, month: m, day: d };
    }
    const mm = m < 1 ? 1 : m > 12 ? 12 : m;
    const dd = d < 1 ? 1 : d > daysInMonth(y, mm) ? daysInMonth(y, mm) : d;
    return { year: y, month: mm, day: dd };
  }

  // BalanceISODate.
  const balanceDate = (y, m, d) => fromEpochDays(epochDays(y, m, 1) + d - 1);

  // BalanceISOYearMonth.
  function balanceYearMonth(y, m) {
    const mm = m - 1;
    return { year: y + floor(mm / 12), month: mod(mm, 12) + 1 };
  }

  const dayOfWeek = (y, m, d) => mod(epochDays(y, m, d) + 3, 7) + 1;
  const dayOfYear = (y, m, d) => epochDays(y, m, d) - epochDays(y, 1, 1) + 1;

  // ISO week number and week-numbering year.
  function weekOfYear(y, m, d) {
    const doy = dayOfYear(y, m, d);
    const dow = dayOfWeek(y, m, d);
    let week = floor((doy - dow + 10) / 7);
    let year = y;
    if (week < 1) {
      year = y - 1;
      const jan1 = dayOfWeek(year, 1, 1);
      week = jan1 === 4 || (jan1 === 3 && isLeap(year)) ? 53 : 52;
    } else if (week === 53) {
      const jan1 = dayOfWeek(y, 1, 1);
      if (!(jan1 === 4 || (jan1 === 3 && isLeap(y)))) {
        week = 1;
        year = y + 1;
      }
    }
    return { week, year };
  }

  // ---- limits ----

  const NS_PER_DAY = 86400000000000n;
  const NS_MAX_INSTANT = 8640000000000000000000n;
  const isValidEpochNs = (ns) => ns >= -NS_MAX_INSTANT && ns <= NS_MAX_INSTANT;
  function checkEpochNs(ns) {
    if (!isValidEpochNs(ns)) throw new RangeError("the instant is out of range");
  }

  // Nanoseconds since the epoch of a date-time read as UTC (GetUTCEpochNanoseconds).
  function utcEpochNs(y, m, d, h, mi, s, ms, us, ns) {
    const days = BigIntF(epochDays(y, m, d));
    const t = BigIntF(((h * 60 + mi) * 60 + s) * 1000 + ms) * 1000000n + BigIntF(us * 1000 + ns);
    return days * NS_PER_DAY + t;
  }

  // ISODateTimeWithinLimits.
  function dateTimeWithinLimits(y, m, d, h, mi, s, ms, us, ns) {
    if (y < -271822 || y > 275761) return false;
    const e = utcEpochNs(y, m, d, h, mi, s, ms, us, ns);
    return e > -NS_MAX_INSTANT - NS_PER_DAY && e < NS_MAX_INSTANT + NS_PER_DAY;
  }
  const dateWithinLimits = (y, m, d) => dateTimeWithinLimits(y, m, d, 12, 0, 0, 0, 0, 0);
  function checkDateTime(y, m, d, h, mi, s, ms, us, ns) {
    if (!dateTimeWithinLimits(y, m, d, h, mi, s, ms, us, ns)) throw new RangeError("the date-time is out of range");
  }
  function checkDate(y, m, d) {
    if (!dateWithinLimits(y, m, d)) throw new RangeError("the date is out of range");
  }
  // ISOYearMonthWithinLimits.
  function checkYearMonth(y, m) {
    if (y < -271821 || y > 275760 || (y === -271821 && m < 4) || (y === 275760 && m > 9)) {
      throw new RangeError("the year-month is out of range");
    }
  }

  // ---- time of day ----

  const isValidTime = (h, mi, s, ms, us, ns) =>
    h >= 0 && h <= 23 && mi >= 0 && mi <= 59 && s >= 0 && s <= 59 && ms >= 0 && ms <= 999 && us >= 0 && us <= 999 &&
    ns >= 0 && ns <= 999;

  // RegulateTime.
  function regulateTime(h, mi, s, ms, us, ns, overflow) {
    if (overflow === "reject") {
      if (!isValidTime(h, mi, s, ms, us, ns)) throw new RangeError("the time is out of range");
      return [h, mi, s, ms, us, ns];
    }
    const c = (x, hi) => (x < 0 ? 0 : x > hi ? hi : x);
    return [c(h, 23), c(mi, 59), c(s, 59), c(ms, 999), c(us, 999), c(ns, 999)];
  }

  // Nanoseconds since midnight (BigInt).
  const timeNs = (t) =>
    BigIntF(((t[0] * 60 + t[1]) * 60 + t[2]) * 1000 + t[3]) * 1000000n + BigIntF(t[4] * 1000 + t[5]);

  // A time of day from nanoseconds since midnight: [days carried, h, mi, s, ms, us, ns].
  function balanceTimeNs(total) {
    const days = bdiv(total, NS_PER_DAY);
    let r = total - days * NS_PER_DAY;
    const ns = NumberF(r % 1000n);
    r /= 1000n;
    const us = NumberF(r % 1000n);
    r /= 1000n;
    const ms = NumberF(r % 1000n);
    r /= 1000n;
    const s = NumberF(r % 60n);
    r /= 60n;
    const mi = NumberF(r % 60n);
    const h = NumberF(r / 60n);
    return [NumberF(days), h, mi, s, ms, us, ns];
  }

  // ---- formatting ----

  function pad(n, w) {
    let s = `${n}`;
    while (s.length < w) s = `0${s}`;
    return s;
  }

  // PadISOYear.
  function padYear(y) {
    if (y >= 0 && y <= 9999) return pad(y, 4);
    return `${y < 0 ? "-" : "+"}${pad(abs(y), 6)}`;
  }

  // FormatFractionalSeconds(subSecondNanoseconds, precision).
  function fractional(sub, precision) {
    if (precision === "auto") {
      if (sub === 0) return "";
      let f = pad(sub, 9);
      while (cu(f, f.length - 1) === 48) f = sub_(f, 0, f.length - 1);
      return `.${f}`;
    }
    if (precision === 0) return "";
    return `.${sub_(pad(sub, 9), 0, precision)}`;
  }
  const sub_ = (s, a, b) => sub(s, a, b);

  // FormatTimeString(hour, minute, second, subSecondNanoseconds, precision).
  function formatTime(h, mi, s, subns, precision) {
    const hm = `${pad(h, 2)}:${pad(mi, 2)}`;
    if (precision === "minute") return hm;
    return `${hm}:${pad(s, 2)}${fractional(subns, precision)}`;
  }

  // ToSecondsStringPrecisionRecord(smallestUnit, fractionalDigitCount).
  function precisionRecord(smallest, digits) {
    switch (smallest) {
      case "minute": return { precision: "minute", unit: "minute", increment: 1 };
      case "second": return { precision: 0, unit: "second", increment: 1 };
      case "millisecond": return { precision: 3, unit: "millisecond", increment: 1 };
      case "microsecond": return { precision: 6, unit: "microsecond", increment: 1 };
      case "nanosecond": return { precision: 9, unit: "nanosecond", increment: 1 };
    }
    if (digits === "auto") return { precision: "auto", unit: "nanosecond", increment: 1 };
    if (digits === 0) return { precision: 0, unit: "second", increment: 1 };
    if (digits <= 3) return { precision: digits, unit: "millisecond", increment: 10 ** (3 - digits) };
    if (digits <= 6) return { precision: digits, unit: "microsecond", increment: 10 ** (6 - digits) };
    return { precision: digits, unit: "nanosecond", increment: 10 ** (9 - digits) };
  }

  // The options of toString for times: [precision record, roundingMode].
  function toStringOptions(options, withSmallest) {
    const o = getOptionsObject(options);
    const digits = fractionalDigitsOpt(o);
    const mode = roundingModeOpt(o, "trunc");
    const smallest = unitOpt(o, "smallestUnit", "time", undefined);
    if (smallest === "hour") throw new RangeError("smallestUnit can't be hour here");
    return { o, record: precisionRecord(smallest, digits), mode, withSmallest };
  }

  // FormatCalendarAnnotation.
  function calendarAnnotation(id, show) {
    if (show === "never") return "";
    if (show === "auto" && id === "iso8601") return "";
    return `[${show === "critical" ? "!" : ""}u-ca=${id}]`;
  }

  // FormatOffsetTimeZoneIdentifier / FormatUTCOffsetNanoseconds.
  function formatOffsetNs(ns, minutesOnly) {
    const sign = ns < 0 ? "-" : "+";
    let a = abs(ns);
    const h = floor(a / 3.6e12);
    a -= h * 3.6e12;
    const mi = floor(a / 6e10);
    a -= mi * 6e10;
    const s = floor(a / 1e9);
    const subns = a - s * 1e9;
    if (minutesOnly || (s === 0 && subns === 0)) return `${sign}${pad(h, 2)}:${pad(mi, 2)}`;
    return `${sign}${formatTime(h, mi, s, subns, "auto")}`;
  }

  // ---- ISO 8601 / RFC 9557 parsing ----

  const fail = (s) => {
    throw new RangeError(`invalid ISO 8601 string: ${s}`);
  };
  const isDigit = (c) => c >= 48 && c <= 57;

  // A scanner over a string's code units.
  function scanner(s) {
    return { s, i: 0, n: s.length };
  }
  const peek = (p, k) => cu(p.s, p.i + (k || 0));
  const at = (p, ch) => p.i < p.n && peek(p) === cu(ch, 0);
  function eat(p, ch) {
    if (at(p, ch)) {
      p.i++;
      return true;
    }
    return false;
  }
  function digits(p, n) {
    let v = 0;
    for (let k = 0; k < n; k++) {
      const c = peek(p, k);
      if (!(p.i + k < p.n && isDigit(c))) return undefined;
      v = v * 10 + (c - 48);
    }
    p.i += n;
    return v;
  }
  const isSign = (c) => c === 43 || c === 45;

  // DateYear: four digits or a sign and six (not -000000).
  function year(p) {
    const c = peek(p);
    if (p.i < p.n && isSign(c)) {
      p.i++;
      const y = digits(p, 6);
      if (y === undefined) return undefined;
      if (c === 45 && y === 0) fail(p.s);
      return c === 45 ? -y : y;
    }
    return digits(p, 4);
  }

  // Fraction: "." or "," and 1–9 digits, as nanoseconds.
  function fraction(p) {
    if (!(at(p, ".") || at(p, ","))) return undefined;
    p.i++;
    let f = 0;
    let k = 0;
    while (p.i < p.n && isDigit(peek(p))) {
      if (k === 9) fail(p.s);
      f = f * 10 + (peek(p) - 48);
      k++;
      p.i++;
    }
    if (k === 0) fail(p.s);
    while (k < 9) {
      f *= 10;
      k++;
    }
    return f;
  }

  // TimeSpec: HH[:MM[:SS[.f]]] or HH[MM[SS[.f]]].
  function timeSpec(p) {
    const h = digits(p, 2);
    if (h === undefined) return undefined;
    if (h > 23) fail(p.s);
    let mi = 0, s = 0, f = 0;
    const colon = at(p, ":");
    if (colon) p.i++;
    const m2 = digits(p, 2);
    if (m2 === undefined) {
      if (colon) fail(p.s);
    } else {
      if (m2 > 59) fail(p.s);
      mi = m2;
      if (colon ? eat(p, ":") : p.i < p.n && isDigit(peek(p))) {
        const s2 = digits(p, 2);
        if (s2 === undefined || s2 > 60) fail(p.s);
        s = s2 === 60 ? 59 : s2;
        const fr = fraction(p);
        if (fr !== undefined) f = fr;
      } else if (!colon && (at(p, ":"))) {
        fail(p.s);
      }
    }
    return [h, mi, s, floor(f / 1e6), floor(f / 1e3) % 1000, f % 1000];
  }

  // UTCOffset: ±HH[[:]MM[[:]SS[.f]]] in nanoseconds; `minutesOnly` rejects seconds.
  function utcOffset(p, minutesOnly) {
    const c = peek(p);
    if (!(p.i < p.n && isSign(c))) return undefined;
    const start = p.i;
    p.i++;
    const h = digits(p, 2);
    if (h === undefined || h > 23) fail(p.s);
    let mi = 0, s = 0, f = 0, secs = false;
    const colon = at(p, ":");
    if (colon) p.i++;
    const m2 = digits(p, 2);
    if (m2 === undefined) {
      if (colon) fail(p.s);
    } else {
      if (m2 > 59) fail(p.s);
      mi = m2;
      if (colon ? at(p, ":") : p.i < p.n && isDigit(peek(p))) {
        if (minutesOnly) fail(p.s);
        secs = true;
        if (colon) p.i++;
        const s2 = digits(p, 2);
        if (s2 === undefined || s2 > 59) fail(p.s);
        s = s2;
        const fr = fraction(p);
        if (fr !== undefined) f = fr;
      }
    }
    const ns = ((h * 60 + mi) * 60 + s) * 1e9 + f;
    // A seconds part, even :00, isn't allowed in a time zone (hasSubMinute).
    return { ns: c === 45 ? -ns : ns, text: sub(p.s, start, p.i), hasSubMinute: secs };
  }

  const isAlpha = (c) => (c >= 65 && c <= 90) || (c >= 97 && c <= 122);
  const isLowerKey = (c) => (c >= 97 && c <= 122) || c === 95;

  // Annotations: an optional time zone annotation, then key=value ones.
  function annotations(p, r) {
    let sawCalendar = false;
    let calendarCritical = false;
    let first = true;
    while (at(p, "[")) {
      const start = p.i;
      p.i++;
      const critical = eat(p, "!");
      // A key (lowercase) followed by "=": a key-value annotation.
      let k = p.i;
      while (k < p.n && (isLowerKey(cu(p.s, k)) || isDigit(cu(p.s, k)) || cu(p.s, k) === 45)) k++;
      if (k < p.n && cu(p.s, k) === 61 && k > p.i && isLowerKey(cu(p.s, p.i))) {
        const key = sub(p.s, p.i, k);
        p.i = k + 1;
        const vs = p.i;
        while (p.i < p.n && peek(p) !== 93) p.i++;
        if (p.i >= p.n || p.i === vs) fail(p.s);
        const value = sub(p.s, vs, p.i);
        p.i++;
        if (key === "u-ca") {
          if (!sawCalendar) r.calendar = value;
          else if (critical || calendarCritical) fail(p.s);
          if (critical) calendarCritical = true;
          if (sawCalendar && calendarCritical) fail(p.s);
          sawCalendar = true;
        } else if (critical) {
          fail(p.s);
        }
        first = false;
        continue;
      }
      // A time zone annotation, only first.
      if (!first) fail(p.s);
      const off = utcOffset(p, true);
      let tz;
      if (off !== undefined) {
        tz = off.text;
      } else {
        const ts = p.i;
        while (p.i < p.n && peek(p) !== 93) p.i++;
        tz = sub(p.s, ts, p.i);
        if (!isTimeZoneName(tz)) fail(p.s);
      }
      if (!eat(p, "]")) fail(p.s);
      r.tz = tz;
      r.tzOffset = off !== undefined;
      first = false;
      void start;
      void critical;
    }
  }

  // An IANA-style name: components of [A-Za-z._+-] (not starting with a digit or "-"),
  // separated by "/".
  function isTimeZoneName(tz) {
    if (tz.length === 0) return false;
    let comp = 0;
    for (let i = 0; i < tz.length; i++) {
      const c = cu(tz, i);
      if (c === 47) {
        if (comp === 0) return false;
        comp = 0;
        continue;
      }
      const ok = isAlpha(c) || c === 46 || c === 95 || c === 43 || (comp > 0 && (isDigit(c) || c === 45)) ||
        (comp === 0 && c === 45 && false);
      if (!ok) return false;
      comp++;
    }
    return comp > 0;
  }

  // Date: DateYear-MM-DD or DateYearMMDD.
  function dateSpec(p) {
    const y = year(p);
    if (y === undefined) return undefined;
    const dash = eat(p, "-");
    const m = digits(p, 2);
    if (m === undefined) fail(p.s);
    if (dash && !eat(p, "-")) fail(p.s);
    const d = digits(p, 2);
    if (d === undefined) fail(p.s);
    if (!isValidISODate(y, m, d)) fail(p.s);
    return [y, m, d];
  }

  // A date-time string with optional time, offset or Z, and annotations.
  // Returns { date, time, z, offset, tz, tzOffset, calendar }.
  function parseDateTime(s, wantTime) {
    const p = scanner(s);
    const r = {};
    const d = dateSpec(p);
    if (d === undefined) fail(s);
    r.date = d;
    if (at(p, "T") || at(p, "t") || at(p, " ")) {
      p.i++;
      const t = timeSpec(p);
      if (t === undefined) fail(s);
      r.time = t;
      if (at(p, "Z") || at(p, "z")) {
        p.i++;
        r.z = true;
      } else {
        const off = utcOffset(p, false);
        if (off !== undefined) r.offset = off;
      }
    }
    annotations(p, r);
    if (p.i !== p.n) fail(s);
    void wantTime;
    return r;
  }

  // ParseTemporalTimeString: a time with optional T, or a date-time with a time.
  function parseTimeString(s) {
    let r;
    try {
      r = parseDateTime(s);
    } catch (e) {
      r = undefined;
    }
    if (r !== undefined) {
      if (r.time === undefined || r.z) fail(s);
      return r;
    }
    const p = scanner(s);
    const hasT = at(p, "T") || at(p, "t");
    if (hasT) p.i++;
    const t = timeSpec(p);
    if (t === undefined) fail(s);
    const r2 = { time: t };
    if (at(p, "Z") || at(p, "z")) fail(s);
    const off = utcOffset(p, false);
    if (off !== undefined) r2.offset = off;
    annotations(p, r2);
    if (p.i !== p.n) fail(s);
    if (!hasT) {
      // Without "T" it mustn't also read as a year-month or month-day.
      const body = s;
      if (isYearMonthLike(body) || isMonthDayLike(body)) fail(s);
    }
    return r2;
  }

  // The annotation-less prefix of `s` as a year-month / month-day (for ambiguity checks).
  function stripAnnotations(s) {
    let i = 0;
    while (i < s.length && cu(s, i) !== 91) i++;
    return sub(s, 0, i);
  }
  function isYearMonthLike(s) {
    const t = stripAnnotations(s);
    const p = scanner(t);
    const y = year(p);
    if (y === undefined) return false;
    eat(p, "-");
    const m = digits(p, 2);
    return m !== undefined && m >= 1 && m <= 12 && p.i === p.n;
  }
  function isMonthDayLike(s) {
    const t = stripAnnotations(s);
    const p = scanner(t);
    if (at(p, "-") && peek(p, 1) === 45) p.i += 2;
    const m = digits(p, 2);
    if (m === undefined) return false;
    eat(p, "-");
    const d = digits(p, 2);
    return d !== undefined && p.i === p.n && m >= 1 && m <= 12 && d >= 1 && d <= daysInMonth(2000, m);
  }

  // ParseTemporalYearMonthString.
  function parseYearMonthString(s) {
    try {
      const r = parseDateTime(s);
      if (r.z) fail(s);
      return r;
    } catch (e) {
      const p = scanner(s);
      const y = year(p);
      if (y === undefined) throw e;
      eat(p, "-");
      const m = digits(p, 2);
      if (m === undefined || m < 1 || m > 12) throw e;
      const r = { date: [y, m, undefined] };
      annotations(p, r);
      if (p.i !== p.n) throw e;
      if (r.calendar !== undefined && asciiLower(r.calendar) !== "iso8601") fail(s);
      return r;
    }
  }

  // ParseTemporalMonthDayString.
  function parseMonthDayString(s) {
    try {
      const r = parseDateTime(s);
      if (r.z) fail(s);
      return r;
    } catch (e) {
      const p = scanner(s);
      if (at(p, "-") && peek(p, 1) === 45) p.i += 2;
      const m = digits(p, 2);
      if (m === undefined) throw e;
      eat(p, "-");
      const d = digits(p, 2);
      if (d === undefined || m < 1 || m > 12 || d < 1 || d > daysInMonth(1972, m)) throw e;
      const r = { date: [undefined, m, d] };
      annotations(p, r);
      if (p.i !== p.n) throw e;
      if (r.calendar !== undefined && asciiLower(r.calendar) !== "iso8601") fail(s);
      return r;
    }
  }

  const asciiLower = (s) => I.asciiLower(s);

  // ParseTemporalDurationString: a Duration record (Numbers), or RangeError.
  function parseDurationString(s) {
    const p = scanner(s);
    let sign = 1;
    if (at(p, "-")) {
      sign = -1;
      p.i++;
    } else eat(p, "+");
    if (!(eat(p, "P") || eat(p, "p"))) fail(s);
    const r = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    const dateDes = ["Y", "M", "W", "D"];
    const timeDes = ["H", "M", "S"];
    let any = false;
    let fractionSeen = false;
    let fracNs = 0n;
    let fracUnit = -1;
    let inTime = false;
    let last = -1;
    while (p.i < p.n) {
      if (at(p, "T") || at(p, "t")) {
        if (inTime) fail(s);
        inTime = true;
        p.i++;
        last = -1;
        if (p.i >= p.n) fail(s);
        continue;
      }
      if (fractionSeen) fail(s);
      const st = p.i;
      while (p.i < p.n && isDigit(peek(p))) p.i++;
      if (p.i === st) fail(s);
      const intStr = sub(s, st, p.i);
      let frac;
      if (at(p, ".") || at(p, ",")) {
        if (!inTime) fail(s);
        p.i++;
        const fs = p.i;
        while (p.i < p.n && isDigit(peek(p))) p.i++;
        const len = p.i - fs;
        if (len === 0 || len > 9) fail(s);
        let f = sub(s, fs, p.i);
        while (f.length < 9) f = `${f}0`;
        frac = BigIntF(f);
        fractionSeen = true;
      }
      const c = I.asciiUpper(sub(s, p.i, p.i + 1));
      p.i++;
      const list = inTime ? timeDes : dateDes;
      let k = -1;
      for (let j = 0; j < list.length; j++) if (list[j] === c) k = j;
      if (k < 0 || k <= last) fail(s);
      last = k;
      const idx = inTime ? 4 + k : k;
      r[idx] = NumberF(intStr);
      if (frac !== undefined) {
        fracNs = frac;
        fracUnit = idx;
      }
      any = true;
    }
    if (!any || (inTime && last === -1)) fail(s);
    // A fractional hour or minute spills into the smaller units.
    if (fracUnit >= 0) {
      const unitTotal = [0n, 0n, 0n, 0n, 3600000000000n, 60000000000n, 1000000000n][fracUnit];
      let rest = (fracNs * unitTotal) / 1000000000n;
      for (let u = fracUnit + 1; u < 10; u++) {
        const len = NS[u];
        const q = rest / len;
        r[u] = NumberF(q);
        rest -= q * len;
      }
    }
    for (let j = 0; j < 10; j++) r[j] = r[j] * sign + 0;
    return r;
  }

  // ---- calendars and time zones ----

  // CanonicalizeCalendar: only the ISO 8601 calendar.
  function canonCalendar(id) {
    const l = asciiLower(id);
    if (l !== "iso8601") throw new RangeError(`unsupported calendar ${id}`);
    return "iso8601";
  }

  // ToTemporalCalendarIdentifier.
  function toCalendar(v) {
    if (isObject(v)) {
      const b = brand(v);
      if (b === DATE || b === DATETIME || b === YEARMONTH || b === MONTHDAY || b === ZONED) return calendarOf(v, b);
    }
    if (typeof v !== "string") throw new TypeError("calendar must be a string");
    if (asciiLower(v) === "iso8601") return "iso8601";
    let id;
    try {
      const r = parseAnyISO(v);
      id = r.calendar === undefined ? "iso8601" : r.calendar;
    } catch (e) {
      throw new RangeError(`invalid calendar ${v}`);
    }
    return canonCalendar(id);
  }

  // Any ISO string that may carry a calendar annotation.
  function parseAnyISO(s) {
    const tries = [parseDateTime, parseTimeString, parseYearMonthString, parseMonthDayString];
    for (let i = 0; i < tries.length; i++) {
      try {
        return tries[i](s);
      } catch (e) {
        // next
      }
    }
    return fail(s);
  }

  function calendarOf(o, b) {
    const s = slots(o, b);
    return b === ZONED ? s[2] : s[s.length - 1];
  }

  // GetTemporalCalendarIdentifierWithISODefault(item).
  function bagCalendar(item) {
    const b = brand(item);
    if (b === DATE || b === DATETIME || b === YEARMONTH || b === MONTHDAY || b === ZONED) return calendarOf(item, b);
    const c = item.calendar;
    if (c === undefined) return "iso8601";
    return toCalendar(c);
  }

  // A time zone identifier: "UTC" or ±HH:MM.
  function canonOffsetId(text) {
    const p = scanner(text);
    const o = utcOffset(p, true);
    if (o === undefined || p.i !== p.n) fail(text);
    return formatOffsetNs(o.ns, true);
  }

  // GetAvailableNamedTimeZoneIdentifier: without a time zone database only UTC.
  function namedZone(name) {
    const l = asciiLower(name);
    if (l === "utc" || l === "etc/utc" || l === "etc/gmt") return "UTC";
    throw new RangeError(`unsupported time zone ${name}`);
  }

  // ToTemporalTimeZoneIdentifier.
  function toTimeZone(v) {
    if (isObject(v) && brand(v) === ZONED) return slots(v, ZONED)[1];
    if (typeof v !== "string") throw new TypeError("timeZone must be a string");
    return parseTimeZoneId(v);
  }

  // ParseTemporalTimeZoneString then canonicalize.
  function parseTimeZoneId(s) {
    if (s.length > 0 && isSign(cu(s, 0))) {
      try {
        return canonOffsetId(s);
      } catch (e) {
        // maybe an ISO string
      }
    }
    if (isTimeZoneName(s) && !isDigit(cu(s, 0))) return namedZone(s);
    let r;
    try {
      r = parseAnyISO(s);
    } catch (e) {
      throw new RangeError(`invalid time zone ${s}`);
    }
    if (r.tz !== undefined) return r.tzOffset ? canonOffsetId(r.tz) : namedZone(r.tz);
    if (r.z) return "UTC";
    if (r.offset !== undefined) {
      if (r.offset.hasSubMinute) throw new RangeError("a time zone offset must be whole minutes");
      return formatOffsetNs(r.offset.ns, true);
    }
    throw new RangeError(`invalid time zone ${s}`);
  }

  // The offset of a fixed time zone in nanoseconds.
  function zoneOffset(tz) {
    if (tz === "UTC") return 0;
    const p = scanner(tz);
    return utcOffset(p, true).ns;
  }

  // GetEpochNanosecondsFor (fixed offsets have one possibility).
  function zonedEpochNs(tz, y, m, d, t) {
    const e = utcEpochNs(y, m, d, t[0], t[1], t[2], t[3], t[4], t[5]) - BigIntF(zoneOffset(tz));
    checkEpochNs(e);
    return e;
  }

  // GetISODateTimeFor: [y, m, d, h, mi, s, ms, us, ns].
  function zonedDateTime(tz, epochNs) {
    const local = epochNs + BigIntF(zoneOffset(tz));
    const days = bdiv(local, NS_PER_DAY);
    const t = balanceTimeNs(local - days * NS_PER_DAY);
    const d = fromEpochDays(NumberF(days));
    return [d.year, d.month, d.day, t[1], t[2], t[3], t[4], t[5], t[6]];
  }

  // ---- property bags ----

  const FIELD_ORDER = ["day", "hour", "microsecond", "millisecond", "minute", "month", "monthCode", "nanosecond",
    "offset", "second", "timeZone", "year"];
  const rec = () => I.objectCreate(null);

  // ToMonthCode.
  function toMonthCode(v) {
    const s = toPrimitiveString(v);
    if (s.length !== 3 || cu(s, 0) !== 77 || !isDigit(cu(s, 1)) || !isDigit(cu(s, 2))) {
      throw new RangeError(`invalid monthCode ${s}`);
    }
    if (s === "M00") throw new RangeError(`invalid monthCode ${s}`);
    return s;
  }

  // ToOffsetString.
  function toOffsetString(v) {
    const s = toPrimitiveString(v);
    const p = scanner(s);
    const o = utcOffset(p, false);
    if (o === undefined || p.i !== p.n) throw new RangeError(`invalid offset ${s}`);
    return s;
  }

  function convertField(name, v) {
    switch (name) {
      case "month":
      case "day":
        return toPositiveIntegerT(v);
      case "monthCode":
        return toMonthCode(v);
      case "offset":
        return toOffsetString(v);
      case "timeZone":
        return toTimeZone(v);
      default:
        return toIntegerT(v);
    }
  }

  // PrepareCalendarFields: fields read in alphabetical order, converted as read.
  // `required` is a list or "partial".
  function prepareFields(bag, names, required) {
    const r = rec();
    let any = false;
    for (let i = 0; i < FIELD_ORDER.length; i++) {
      const name = FIELD_ORDER[i];
      if (!includes(names, name)) continue;
      const v = bag[name];
      if (v !== undefined) {
        any = true;
        r[name] = convertField(name, v);
      } else if (required !== "partial" && includes(required, name)) {
        throw new TypeError(`${name} is required`);
      }
    }
    if (required === "partial" && !any) throw new TypeError("no recognized properties");
    return r;
  }

  const DATE_FIELDS = ["day", "month", "monthCode", "year"];
  const TIME_FIELDS = ["hour", "microsecond", "millisecond", "minute", "nanosecond", "second"];

  // The month of fields with month and/or monthCode (ISO calendar).
  function resolveMonth(f) {
    let month = f.month;
    if (f.monthCode !== undefined) {
      const mc = NumberF(sub(f.monthCode, 1, 3));
      if (mc < 1 || mc > 12) throw new RangeError(`invalid monthCode ${f.monthCode}`);
      if (month !== undefined && month !== mc) throw new RangeError("month and monthCode disagree");
      month = mc;
    }
    if (month === undefined) throw new TypeError("month or monthCode is required");
    return month;
  }

  // CalendarDateFromFields (ISO).
  function dateFromFields(f, overflow) {
    if (f.year === undefined) throw new TypeError("year is required");
    if (f.day === undefined) throw new TypeError("day is required");
    const month = resolveMonth(f);
    const d = regulateDate(f.year, month, f.day, overflow);
    checkDate(d.year, d.month, d.day);
    return d;
  }

  // The time of fields (missing ones 0), regulated.
  function timeFromFields(f, overflow) {
    const g = (k) => (f[k] === undefined ? 0 : f[k]);
    return regulateTime(g("hour"), g("minute"), g("second"), g("millisecond"), g("microsecond"), g("nanosecond"),
      overflow);
  }

  // ToTemporalTimeRecord(bag, partial): [h, mi, s, ms, us, ns] (undefined for missing in partial).
  function timeRecord(bag, partial) {
    const keys = TIME_FIELDS;
    const out = rec();
    let any = false;
    for (let i = 0; i < keys.length; i++) {
      const v = bag[keys[i]];
      if (v !== undefined) {
        any = true;
        out[keys[i]] = toIntegerT(v);
      }
    }
    if (!any) throw new TypeError("no time properties");
    void partial;
    return out;
  }

  // ---- durations ----

  const TWO32 = 4294967296;
  const MAX_TIME = 9007199254740992n * 1000000000n;

  // TimeDurationFromComponents (exact).
  function timeDuration(h, mi, s, ms, us, ns) {
    return BigIntF(h) * 3600000000000n + BigIntF(mi) * 60000000000n + BigIntF(s) * 1000000000n +
      BigIntF(ms) * 1000000n + BigIntF(us) * 1000n + BigIntF(ns);
  }

  // IsValidDuration.
  function isValidDuration(r) {
    let sign = 0;
    for (let i = 0; i < 10; i++) {
      const v = r[i];
      if (!isFiniteN(v) || trunc(v) !== v) return false;
      const s = v < 0 ? -1 : v > 0 ? 1 : 0;
      if (s !== 0) {
        if (sign !== 0 && s !== sign) return false;
        sign = s;
      }
    }
    if (abs(r[0]) >= TWO32 || abs(r[1]) >= TWO32 || abs(r[2]) >= TWO32) return false;
    const t = BigIntF(r[3]) * NS_PER_DAY + timeDuration(r[4], r[5], r[6], r[7], r[8], r[9]);
    return babs(t) < MAX_TIME;
  }

  function durationSign(r) {
    for (let i = 0; i < 10; i++) {
      if (r[i] < 0) return -1;
      if (r[i] > 0) return 1;
    }
    return 0;
  }

  // CreateTemporalDuration.
  function makeDuration(r, proto) {
    const v = [];
    for (let i = 0; i < 10; i++) v[i] = r[i] + 0;
    if (!isValidDuration(v)) throw new RangeError("the duration is out of range");
    return create(DURATION, v, proto);
  }

  // ToTemporalPartialDurationRecord: the fields read in alphabetical order.
  const DURATION_ORDER = [["days", 3], ["hours", 4], ["microseconds", 8], ["milliseconds", 7], ["minutes", 5],
    ["months", 1], ["nanoseconds", 9], ["seconds", 6], ["weeks", 2], ["years", 0]];
  function partialDuration(bag) {
    if (!isObject(bag)) throw new TypeError("a duration-like object is required");
    const r = [undefined, undefined, undefined, undefined, undefined, undefined, undefined, undefined, undefined,
      undefined];
    let any = false;
    for (let i = 0; i < DURATION_ORDER.length; i++) {
      const e = DURATION_ORDER[i];
      const v = bag[e[0]];
      if (v !== undefined) {
        any = true;
        r[e[1]] = toIntegerIfIntegral(v);
      }
    }
    if (!any) throw new TypeError("no duration properties");
    return r;
  }

  // ToTemporalDuration: a Duration record.
  function toDurationRecord(item) {
    if (isObject(item) && brand(item) === DURATION) {
      const s = slots(item, DURATION);
      const r = [];
      for (let i = 0; i < 10; i++) r[i] = s[i];
      return r;
    }
    if (!isObject(item)) {
      if (typeof item !== "string") throw new TypeError("a duration must be a string or an object");
      const r = parseDurationString(item);
      if (!isValidDuration(r)) throw new RangeError("the duration is out of range");
      return r;
    }
    const p = partialDuration(item);
    const r = [];
    for (let i = 0; i < 10; i++) r[i] = p[i] === undefined ? 0 : p[i];
    if (!isValidDuration(r)) throw new RangeError("the duration is out of range");
    return r;
  }

  // DefaultTemporalLargestUnit.
  function defaultLargest(r) {
    for (let i = 0; i < 10; i++) if (r[i] !== 0) return UNITS[i];
    return "nanosecond";
  }

  // An internal duration: { date: [y, mo, w, d], time: BigInt ns }.
  const internal = (r) => ({ date: [r[0], r[1], r[2], r[3]], time: timeDuration(r[4], r[5], r[6], r[7], r[8], r[9]) });
  const internal24 = (r) => ({ date: [r[0], r[1], r[2], 0], time: BigIntF(r[3]) * NS_PER_DAY + timeDuration(r[4], r[5], r[6], r[7], r[8], r[9]) });
  const internalSign = (d) => {
    for (let i = 0; i < 4; i++) if (d.date[i] !== 0) return d.date[i] < 0 ? -1 : 1;
    return bsign(d.time);
  };
  const zeroInternal = () => ({ date: [0, 0, 0, 0], time: 0n });

  // TemporalDurationFromInternal.
  function fromInternal(d, largest) {
    const sign = bsign(d.time);
    let t = babs(d.time);
    let days = 0n, hours = 0n, minutes = 0n, seconds = 0n, ms = 0n, us = 0n;
    const li = unitIndex(largest);
    if (li <= 3) {
      us = t / 1000n; t %= 1000n; ms = us / 1000n; us %= 1000n; seconds = ms / 1000n; ms %= 1000n;
      minutes = seconds / 60n; seconds %= 60n; hours = minutes / 60n; minutes %= 60n; days = hours / 24n; hours %= 24n;
    } else if (li === 4) {
      us = t / 1000n; t %= 1000n; ms = us / 1000n; us %= 1000n; seconds = ms / 1000n; ms %= 1000n;
      minutes = seconds / 60n; seconds %= 60n; hours = minutes / 60n; minutes %= 60n;
    } else if (li === 5) {
      us = t / 1000n; t %= 1000n; ms = us / 1000n; us %= 1000n; seconds = ms / 1000n; ms %= 1000n;
      minutes = seconds / 60n; seconds %= 60n;
    } else if (li === 6) {
      us = t / 1000n; t %= 1000n; ms = us / 1000n; us %= 1000n; seconds = ms / 1000n; ms %= 1000n;
    } else if (li === 7) {
      us = t / 1000n; t %= 1000n; ms = us / 1000n; us %= 1000n;
    } else if (li === 8) {
      us = t / 1000n; t %= 1000n;
    }
    const s = (x) => NumberF(x) * sign + 0;
    return [d.date[0], d.date[1], d.date[2], d.date[3] + s(days), s(hours), s(minutes), s(seconds), s(ms), s(us), s(t)];
  }

  // ---- date differences and rounding (§ Duration rounding) ----

  const cmpN = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
  const compareDate = (a, b) => cmpN(a[0], b[0]) || cmpN(a[1], b[1]) || cmpN(a[2], b[2]);
  const addDays = (d, n) => {
    const r = fromEpochDays(epochDays(d[0], d[1], d[2]) + n);
    return [r.year, r.month, r.day];
  };

  // ISODateSurpasses.
  function surpasses(sign, y1, m1, d1, two) {
    if (y1 !== two[0]) return sign * (y1 - two[0]) > 0;
    if (m1 !== two[1]) return sign * (m1 - two[1]) > 0;
    if (d1 !== two[2]) return sign * (d1 - two[2]) > 0;
    return false;
  }

  // CalendarDateUntil (ISO): [years, months, weeks, days].
  function dateUntil(one, two, largest) {
    const li = unitIndex(largest);
    if (li <= 1) {
      const sign = -compareDate(one, two);
      if (sign === 0) return [0, 0, 0, 0];
      let years = 0;
      if (li === 0) {
        let cand = two[0] - one[0] - sign;
        if (sign * cand < 0) cand = 0;
        years = cand;
        while (!surpasses(sign, one[0] + cand, one[1], one[2], two)) {
          years = cand;
          cand += sign;
        }
      }
      let months = 0;
      let candM = sign;
      let ym = balanceYearMonth(one[0] + years, one[1] + candM);
      while (!surpasses(sign, ym.year, ym.month, one[2], two)) {
        months = candM;
        candM += sign;
        ym = balanceYearMonth(one[0] + years, one[1] + candM);
      }
      if (li === 1) {
        months += years * 12;
        years = 0;
      }
      const im = balanceYearMonth(one[0] + years, one[1] + months);
      const c = regulateDate(im.year, im.month, one[2], "constrain");
      const days = epochDays(two[0], two[1], two[2]) - epochDays(c.year, c.month, c.day);
      return [years, months, 0, days];
    }
    let days = epochDays(two[0], two[1], two[2]) - epochDays(one[0], one[1], one[2]);
    let weeks = 0;
    if (li === 2) {
      weeks = trunc(days / 7) + 0;
      days = days - weeks * 7 + 0;
    }
    return [0, 0, weeks, days];
  }

  // CalendarDateAdd (ISO): [y, m, d].
  function dateAdd(date, dd, overflow) {
    const ym = balanceYearMonth(date[0] + dd[0], date[1] + dd[1]);
    const r = regulateDate(ym.year, ym.month, date[2], overflow);
    const out = addDays([r.year, r.month, r.day], dd[3] + 7 * dd[2]);
    checkDate(out[0], out[1], out[2]);
    return out;
  }

  // The epoch nanoseconds of date + time, in `tz` or as UTC when tz is undefined.
  function epochOf(date, t, tz) {
    if (tz === undefined) return utcEpochNs(date[0], date[1], date[2], t[0], t[1], t[2], t[3], t[4], t[5]);
    return zonedEpochNs(tz, date[0], date[1], date[2], t);
  }

  // ApplyUnsignedRoundingMode on x = r1 + (num/den)·(r2 − r1): true for r2.
  function pickR2(num, den, um, r1Even) {
    if (num === 0n) return false;
    if (um === "zero") return false;
    if (um === "infinity") return true;
    const twice = num * 2n;
    if (twice < den) return false;
    if (twice > den) return true;
    if (um === "half-zero") return false;
    if (um === "half-infinity") return true;
    return !r1Even;
  }

  // NudgeToCalendarUnit: { duration, nudged, expanded, total }.
  function nudgeCalendar(sign, d, dest, date, time, tz, inc, unit, mode) {
    const y = d.date[0], mo = d.date[1], w = d.date[2], dy = d.date[3];
    let r1, r2, start, end;
    const rt = (x) => NumberF(roundBig(BigIntF(x), BigIntF(inc), "trunc"));
    if (unit === "year") {
      r1 = rt(y); r2 = r1 + inc * sign; start = [r1, 0, 0, 0]; end = [r2, 0, 0, 0];
    } else if (unit === "month") {
      r1 = rt(mo); r2 = r1 + inc * sign; start = [y, r1, 0, 0]; end = [y, r2, 0, 0];
    } else if (unit === "week") {
      const ws = dateAdd(date, [y, mo, 0, 0], "constrain");
      const we = addDays(ws, dy);
      const u = dateUntil(ws, we, "week");
      r1 = rt(w + u[2]); r2 = r1 + inc * sign; start = [y, mo, r1, 0]; end = [y, mo, r2, 0];
    } else {
      r1 = rt(dy); r2 = r1 + inc * sign; start = [y, mo, w, r1]; end = [y, mo, w, r2];
    }
    const sd = dateAdd(date, start, "constrain");
    const ed = dateAdd(date, end, "constrain");
    const startNs = epochOf(sd, time, tz);
    const endNs = epochOf(ed, time, tz);
    if (sign === 1 ? (startNs > dest || dest > endNs) : (startNs < dest || dest < endNs)) {
      throw new RangeError("the duration can't be rounded relative to this date");
    }
    const num = babs(dest - startNs);
    const den = babs(endNs - startNs);
    const total = r1 + (NumberF(num) / NumberF(den)) * inc * sign;
    const um = unsignedMode(mode, sign < 0);
    const r1Even = mod(abs(r1) / inc, 2) === 0;
    // progress = 1 (the destination is the end) rounds to r2.
    const expand = num === den || pickR2(num, den, um, r1Even);
    return {
      duration: { date: expand ? end : start, time: 0n },
      nudged: expand ? endNs : startNs,
      expanded: expand,
      total,
    };
  }

  // NudgeToZonedTime.
  function nudgeZoned(sign, d, date, time, tz, inc, unit, mode) {
    const start = dateAdd(date, d.date, "constrain");
    const endD = addDays(start, sign);
    const startNs = epochOf(start, time, tz);
    const endNs = epochOf(endD, time, tz);
    const span = endNs - startNs;
    const len = unitNs(unit) * BigIntF(inc);
    let rounded = roundBig(d.time, len, mode);
    const beyond = rounded - span;
    let expanded, dayDelta, nudged;
    if (bsign(beyond) !== -sign) {
      expanded = true;
      dayDelta = sign;
      rounded = roundBig(beyond, len, mode);
      nudged = endNs + rounded;
    } else {
      expanded = false;
      dayDelta = 0;
      nudged = startNs + rounded;
    }
    return { duration: { date: [d.date[0], d.date[1], d.date[2], d.date[3] + dayDelta], time: rounded }, nudged, expanded };
  }

  // NudgeToDayOrTime.
  function nudgeDayOrTime(d, dest, largest, inc, unit, mode) {
    const t = d.time + BigIntF(d.date[3]) * NS_PER_DAY;
    const rounded = roundBig(t, unitNs(unit) * BigIntF(inc), mode);
    const diff = rounded - t;
    const whole = t / NS_PER_DAY;
    const roundedWhole = rounded / NS_PER_DAY;
    const delta = roundedWhole - whole;
    const expanded = delta !== 0n && bsign(delta) === bsign(t);
    const nudged = dest + diff;
    let days = 0;
    let rem = rounded;
    if (unitIndex(largest) <= 3) {
      days = NumberF(roundedWhole);
      rem = rounded - roundedWhole * NS_PER_DAY;
    }
    return { duration: { date: [d.date[0], d.date[1], d.date[2], days], time: rem }, nudged, expanded };
  }

  // BubbleRelativeDuration.
  function bubble(sign, d, nudged, date, time, tz, largest, smallest) {
    if (smallest === largest) return d;
    const li = unitIndex(largest);
    const si = unitIndex(smallest);
    for (let ui = si - 1; ui >= li; ui--) {
      const unit = UNITS[ui];
      if (unit === "week" && largest !== "week") continue;
      let endD;
      if (unit === "year") endD = [d.date[0] + sign, 0, 0, 0];
      else if (unit === "month") endD = [d.date[0], d.date[1] + sign, 0, 0];
      else if (unit === "week") endD = [d.date[0], d.date[1], d.date[2] + sign, 0];
      else continue;
      const end = dateAdd(date, endD, "constrain");
      const endNs = epochOf(end, time, tz);
      const beyond = nudged - endNs;
      if (bsign(beyond) !== -sign) d = { date: endD, time: 0n };
      else break;
    }
    return d;
  }

  // RoundRelativeDuration.
  function roundRelative(d, dest, date, time, tz, largest, inc, smallest, mode) {
    const irregular = unitIndex(smallest) <= 2 || (tz !== undefined && smallest === "day");
    const sign = internalSign(d) < 0 ? -1 : 1;
    let n;
    if (irregular) n = nudgeCalendar(sign, d, dest, date, time, tz, inc, smallest, mode);
    else if (tz !== undefined) n = nudgeZoned(sign, d, date, time, tz, inc, smallest, mode);
    else n = nudgeDayOrTime(d, dest, largest, inc, smallest, mode);
    let out = n.duration;
    if (n.expanded && smallest !== "week") {
      out = bubble(sign, out, n.nudged, date, time, tz, largest, largerUnit(smallest, "day"));
    }
    return out;
  }

  // TotalTimeDuration.
  function totalTime(t, unit) {
    const u = unitNs(unit);
    const q = t / u;
    const r = t % u;
    return NumberF(q) + NumberF(r) / NumberF(u);
  }

  // TotalRelativeDuration.
  function totalRelative(d, dest, date, time, tz, unit) {
    if (unitIndex(unit) <= 2 || (tz !== undefined && unit === "day")) {
      const sign = internalSign(d) < 0 ? -1 : 1;
      return nudgeCalendar(sign, d, dest, date, time, tz, 1, unit, "trunc").total;
    }
    return totalTime(d.time + BigIntF(d.date[3]) * NS_PER_DAY, unit);
  }

  // DifferenceISODateTime: an internal duration.
  function diffDateTime(d1, t1, d2, t2, largest) {
    let time = timeNs(t2) - timeNs(t1);
    const timeSign = bsign(time);
    const dateSign = compareDate(d2, d1);
    let adj = d2;
    if (timeSign === -dateSign && timeSign !== 0) {
      adj = addDays(adj, timeSign);
      time -= BigIntF(timeSign) * NS_PER_DAY;
    }
    const dl = largerUnit("day", largest);
    const dd = dateUntil(d1, adj, dl);
    if (largest !== dl) {
      time += BigIntF(dd[3]) * NS_PER_DAY;
      dd[3] = 0;
    }
    return { date: dd, time };
  }

  // GetDifferenceSettings.
  function differenceSettings(since, o, group, disallowed, fallbackSmallest, defaultLargestFloor) {
    let largest = unitOpt(o, "largestUnit", group, "auto", ["auto"]);
    if (includes(disallowed, largest)) throw new RangeError(`${largest} isn't allowed here`);
    const inc = roundingIncrementOpt(o);
    let mode = roundingModeOpt(o, "trunc");
    if (since) mode = negateMode(mode);
    const smallest = unitOpt(o, "smallestUnit", group, fallbackSmallest);
    if (includes(disallowed, smallest)) throw new RangeError(`${smallest} isn't allowed here`);
    const defLargest = largerUnit(defaultLargestFloor, smallest);
    if (largest === "auto") largest = defLargest;
    if (largerUnit(largest, smallest) !== largest) throw new RangeError("largestUnit is smaller than smallestUnit");
    const max = MAXIMUM[smallest];
    if (max !== undefined) validateIncrement(inc, max, false);
    return { largest, smallest, inc, mode };
  }

  function negateMode(m) {
    switch (m) {
      case "ceil": return "floor";
      case "floor": return "ceil";
      case "halfCeil": return "halfFloor";
      case "halfFloor": return "halfCeil";
      default: return m;
    }
  }

  const negateRecord = (r) => {
    const out = [];
    for (let i = 0; i < 10; i++) out[i] = -r[i] + 0;
    return out;
  };

  // ---- shared object helpers ----

  const PLAIN_BRANDS = [DATE, DATETIME, MONTHDAY, TIME, YEARMONTH, ZONED];

  // IsPartialTemporalObject.
  function checkPartial(v) {
    if (!isObject(v)) throw new TypeError("a partial Temporal object is required");
    if (includes(PLAIN_BRANDS, brand(v))) throw new TypeError("a Temporal object can't be a partial object");
    if (v.calendar !== undefined) throw new TypeError("calendar isn't allowed here");
    if (v.timeZone !== undefined) throw new TypeError("timeZone isn't allowed here");
  }

  // round options as an object (a string is smallestUnit).
  function roundToOptions(roundTo) {
    if (roundTo === undefined) throw new TypeError("options are required");
    if (typeof roundTo === "string") {
      const o = rec();
      I.defineValue(o, "smallestUnit", roundTo, true, true, true);
      return o;
    }
    return getOptionsObject(roundTo);
  }

  // RoundTime: [days, h, mi, s, ms, us, ns].
  function roundTime(t, inc, unit, mode) {
    const total = timeNs(t);
    const len = (unit === "day" ? NS_PER_DAY : unitNs(unit)) * BigIntF(inc);
    return balanceTimeNs(roundBig(total, len, mode));
  }

  const dtArr = (date, t) => [date[0], date[1], date[2], t[0], t[1], t[2], t[3], t[4], t[5]];
  const dtDate = (a) => [a[0], a[1], a[2]];
  const dtTime = (a) => [a[3], a[4], a[5], a[6], a[7], a[8]];
  const compareDT = (a, b) => {
    for (let i = 0; i < 9; i++) {
      const c = cmpN(a[i], b[i]);
      if (c !== 0) return c;
    }
    return 0;
  };

  // ---- PlainTime ----

  function makeTime(t, proto) {
    return create(TIME, [t[0], t[1], t[2], t[3], t[4], t[5]], proto);
  }
  const timeOf = (o) => {
    const s = slots(o, TIME);
    return [s[0], s[1], s[2], s[3], s[4], s[5]];
  };

  // ToTemporalTime: a time record.
  function toTime(item, options) {
    if (isObject(item)) {
      const b = brand(item);
      if (b === TIME || b === DATETIME || b === ZONED) {
        overflowOpt(getOptionsObject(options));
        if (b === TIME) return timeOf(item);
        if (b === DATETIME) return dtTime(slots(item, DATETIME));
        const z = slots(item, ZONED);
        return dtTime(zonedDateTime(z[1], z[0]));
      }
      const f = timeRecord(item);
      const overflow = overflowOpt(getOptionsObject(options));
      return timeFromFields(f, overflow);
    }
    if (typeof item !== "string") throw new TypeError("a time must be a string or an object");
    const r = parseTimeString(item);
    if (r.calendar !== undefined) canonCalendar(r.calendar);
    overflowOpt(getOptionsObject(options));
    return r.time;
  }

  // ToTimeRecordOrMidnight.
  const timeOrMidnight = (v) => (v === undefined ? [0, 0, 0, 0, 0, 0] : toTime(v));

  function timeToString(t, precision) {
    return formatTime(t[0], t[1], t[2], t[3] * 1e6 + t[4] * 1e3 + t[5], precision);
  }

  class PlainTime {
    constructor(hour = undefined, minute = undefined, second = undefined, millisecond = undefined,
      microsecond = undefined, nanosecond = undefined) {
      const v = (x) => (x === undefined ? 0 : toIntegerT(x));
      const t = [v(hour), v(minute), v(second), v(millisecond), v(microsecond), v(nanosecond)];
      if (!isValidTime(t[0], t[1], t[2], t[3], t[4], t[5])) throw new RangeError("the time is out of range");
      I.setSlots(this, TIME, t);
    }
    static from(item, options = undefined) {
      return makeTime(toTime(item, options));
    }
    static compare(one, two) {
      return compareTimes(toTime(one), toTime(two));
    }
    get hour() { return slots(this, TIME)[0]; }
    get minute() { return slots(this, TIME)[1]; }
    get second() { return slots(this, TIME)[2]; }
    get millisecond() { return slots(this, TIME)[3]; }
    get microsecond() { return slots(this, TIME)[4]; }
    get nanosecond() { return slots(this, TIME)[5]; }
    add(temporalDurationLike) {
      return addToTime(this, temporalDurationLike, 1);
    }
    subtract(temporalDurationLike) {
      return addToTime(this, temporalDurationLike, -1);
    }
    with(temporalTimeLike, options = undefined) {
      const t = timeOf(this);
      checkPartial(temporalTimeLike);
      const p = timeRecord(temporalTimeLike, true);
      const g = (k, i) => (p[k] === undefined ? t[i] : p[k]);
      const overflow = overflowOpt(getOptionsObject(options));
      return makeTime(regulateTime(g("hour", 0), g("minute", 1), g("second", 2), g("millisecond", 3),
        g("microsecond", 4), g("nanosecond", 5), overflow));
    }
    until(other, options = undefined) {
      return diffTimes(false, this, other, options);
    }
    since(other, options = undefined) {
      return diffTimes(true, this, other, options);
    }
    round(roundTo) {
      const t = timeOf(this);
      const o = roundToOptions(roundTo);
      const inc = roundingIncrementOpt(o);
      const mode = roundingModeOpt(o, "halfExpand");
      const unit = unitOpt(o, "smallestUnit", "time", "required");
      validateIncrement(inc, MAXIMUM[unit], false);
      const r = roundTime(t, inc, unit, mode);
      return makeTime([r[1], r[2], r[3], r[4], r[5], r[6]]);
    }
    equals(other) {
      const t = timeOf(this);
      return compareTimes(t, toTime(other)) === 0;
    }
    toString(options = undefined) {
      const t = timeOf(this);
      const o = toStringOptions(options);
      const r = roundTime(t, o.record.increment, o.record.unit, o.mode);
      return timeToString([r[1], r[2], r[3], r[4], r[5], r[6]], o.record.precision);
    }
    toLocaleString(locales = undefined, options = undefined) {
      return timeToString(timeOf(this), "auto");
    }
    toJSON() {
      return timeToString(timeOf(this), "auto");
    }
    valueOf() {
      throw new TypeError("use compare() or equals() to compare Temporal.PlainTime");
    }
  }

  function compareTimes(a, b) {
    for (let i = 0; i < 6; i++) {
      const c = cmpN(a[i], b[i]);
      if (c !== 0) return c;
    }
    return 0;
  }

  function addToTime(o, like, sign) {
    const t = timeOf(o);
    let r = toDurationRecord(like);
    if (sign < 0) r = negateRecord(r);
    const d = internal(r);
    const b = balanceTimeNs(timeNs(t) + d.time);
    return makeTime([b[1], b[2], b[3], b[4], b[5], b[6]]);
  }

  function diffTimes(since, o, other, options) {
    const t = timeOf(o);
    const u = toTime(other);
    const opts = getOptionsObject(options);
    const s = differenceSettings(since, opts, "time", [], "nanosecond", "hour");
    let td = timeNs(u) - timeNs(t);
    td = roundBig(td, unitNs(s.smallest) * BigIntF(s.inc), s.mode);
    let r = fromInternal({ date: [0, 0, 0, 0], time: td }, s.largest);
    if (since) r = negateRecord(r);
    return makeDuration(r);
  }

  // ---- PlainDate ----

  function makeDate(d, cal, proto) {
    checkDate(d[0], d[1], d[2]);
    return create(DATE, [d[0], d[1], d[2], cal], proto);
  }
  const dateOf = (o) => {
    const s = slots(o, DATE);
    return [s[0], s[1], s[2]];
  };

  // ToTemporalDate: [date, calendar].
  function toDate(item, options) {
    if (isObject(item)) {
      const b = brand(item);
      if (b === DATE || b === DATETIME || b === ZONED) {
        overflowOpt(getOptionsObject(options));
        if (b === DATE) {
          const s = slots(item, DATE);
          return [[s[0], s[1], s[2]], s[3]];
        }
        if (b === DATETIME) {
          const s = slots(item, DATETIME);
          return [[s[0], s[1], s[2]], s[9]];
        }
        const z = slots(item, ZONED);
        return [dtDate(zonedDateTime(z[1], z[0])), z[2]];
      }
      const cal = bagCalendar(item);
      const f = prepareFields(item, DATE_FIELDS, []);
      const overflow = overflowOpt(getOptionsObject(options));
      const d = dateFromFields(f, overflow);
      return [[d.year, d.month, d.day], cal];
    }
    if (typeof item !== "string") throw new TypeError("a date must be a string or an object");
    const r = parseDateTime(item);
    if (r.z) throw new RangeError("a UTC designator isn't allowed for a plain date");
    const cal = r.calendar === undefined ? "iso8601" : canonCalendar(r.calendar);
    overflowOpt(getOptionsObject(options));
    checkDate(r.date[0], r.date[1], r.date[2]);
    return [r.date, cal];
  }

  function dateToString(d, cal, show) {
    return `${padYear(d[0])}-${pad(d[1], 2)}-${pad(d[2], 2)}${calendarAnnotation(cal, show)}`;
  }

  const monthCode = (m) => `M${pad(m, 2)}`;

  // CalendarMergeFields (ISO): month and monthCode replace each other.
  function mergeFields(base, extra) {
    const r = rec();
    const keys = FIELD_ORDER;
    const replacesMonth = extra.month !== undefined || extra.monthCode !== undefined;
    for (let i = 0; i < keys.length; i++) {
      const k = keys[i];
      if (extra[k] !== undefined) r[k] = extra[k];
      else if (!(replacesMonth && (k === "month" || k === "monthCode")) && base[k] !== undefined) r[k] = base[k];
    }
    return r;
  }

  function dateFields(d) {
    const f = rec();
    f.year = d[0];
    f.month = d[1];
    f.monthCode = monthCode(d[1]);
    f.day = d[2];
    return f;
  }

  // ToDateDurationRecordWithoutTime: [y, mo, w, d].
  function dateDurationOf(r) {
    const i = internal24(r);
    return [i.date[0], i.date[1], i.date[2], NumberF(i.time / NS_PER_DAY)];
  }

  class PlainDate {
    constructor(isoYear, isoMonth, isoDay, calendar = undefined) {
      const y = toIntegerT(isoYear);
      const m = toIntegerT(isoMonth);
      const d = toIntegerT(isoDay);
      let cal = "iso8601";
      if (calendar !== undefined) {
        if (typeof calendar !== "string") throw new TypeError("calendar must be a string");
        cal = canonCalendar(calendar);
      }
      if (!isValidISODate(y, m, d)) throw new RangeError("the date is out of range");
      checkDate(y, m, d);
      I.setSlots(this, DATE, [y, m, d, cal]);
    }
    static from(item, options = undefined) {
      const r = toDate(item, options);
      return makeDate(r[0], r[1]);
    }
    static compare(one, two) {
      return compareDate(toDate(one)[0], toDate(two)[0]);
    }
    get calendarId() { return slots(this, DATE)[3]; }
    get era() { slots(this, DATE); return undefined; }
    get eraYear() { slots(this, DATE); return undefined; }
    get year() { return slots(this, DATE)[0]; }
    get month() { return slots(this, DATE)[1]; }
    get monthCode() { return monthCode(slots(this, DATE)[1]); }
    get day() { return slots(this, DATE)[2]; }
    get dayOfWeek() { const d = dateOf(this); return dayOfWeek(d[0], d[1], d[2]); }
    get dayOfYear() { const d = dateOf(this); return dayOfYear(d[0], d[1], d[2]); }
    get weekOfYear() { const d = dateOf(this); return weekOfYear(d[0], d[1], d[2]).week; }
    get yearOfWeek() { const d = dateOf(this); return weekOfYear(d[0], d[1], d[2]).year; }
    get daysInWeek() { slots(this, DATE); return 7; }
    get daysInMonth() { const d = dateOf(this); return daysInMonth(d[0], d[1]); }
    get daysInYear() { return daysInYear(dateOf(this)[0]); }
    get monthsInYear() { slots(this, DATE); return 12; }
    get inLeapYear() { return isLeap(dateOf(this)[0]); }
    toPlainYearMonth() {
      const s = slots(this, DATE);
      return makeYearMonth([s[0], s[1], 1], s[3]);
    }
    toPlainMonthDay() {
      const s = slots(this, DATE);
      return makeMonthDay([1972, s[1], s[2]], s[3]);
    }
    add(temporalDurationLike, options = undefined) {
      return addToDate(this, temporalDurationLike, options, 1);
    }
    subtract(temporalDurationLike, options = undefined) {
      return addToDate(this, temporalDurationLike, options, -1);
    }
    with(temporalDateLike, options = undefined) {
      const s = slots(this, DATE);
      checkPartial(temporalDateLike);
      const base = dateFields([s[0], s[1], s[2]]);
      const p = prepareFields(temporalDateLike, DATE_FIELDS, "partial");
      const f = mergeFields(base, p);
      const overflow = overflowOpt(getOptionsObject(options));
      const d = dateFromFields(f, overflow);
      return makeDate([d.year, d.month, d.day], s[3]);
    }
    withCalendar(calendarLike) {
      const s = slots(this, DATE);
      return makeDate([s[0], s[1], s[2]], toCalendar(calendarLike));
    }
    until(other, options = undefined) {
      return diffDates(false, this, other, options);
    }
    since(other, options = undefined) {
      return diffDates(true, this, other, options);
    }
    equals(other) {
      const s = slots(this, DATE);
      const o = toDate(other);
      return compareDate([s[0], s[1], s[2]], o[0]) === 0 && s[3] === o[1];
    }
    toPlainDateTime(temporalTime = undefined) {
      const s = slots(this, DATE);
      const t = timeOrMidnight(temporalTime);
      return makeDateTime(dtArr([s[0], s[1], s[2]], t), s[3]);
    }
    toZonedDateTime(item) {
      const s = slots(this, DATE);
      let tz, time;
      if (isObject(item)) {
        const tzLike = item.timeZone;
        if (tzLike === undefined) tz = toTimeZone(item);
        else {
          tz = toTimeZone(tzLike);
          time = item.plainTime;
        }
      } else tz = toTimeZone(item);
      const d = [s[0], s[1], s[2]];
      let ns;
      if (time === undefined) ns = zonedEpochNs(tz, d[0], d[1], d[2], [0, 0, 0, 0, 0, 0]);
      else {
        const t = toTime(time);
        checkDateTime(d[0], d[1], d[2], t[0], t[1], t[2], t[3], t[4], t[5]);
        ns = zonedEpochNs(tz, d[0], d[1], d[2], t);
      }
      return makeZoned(ns, tz, s[3]);
    }
    toString(options = undefined) {
      const s = slots(this, DATE);
      const show = calendarNameOpt(getOptionsObject(options));
      return dateToString([s[0], s[1], s[2]], s[3], show);
    }
    toLocaleString(locales = undefined, options = undefined) {
      const s = slots(this, DATE);
      return dateToString([s[0], s[1], s[2]], s[3], "auto");
    }
    toJSON() {
      const s = slots(this, DATE);
      return dateToString([s[0], s[1], s[2]], s[3], "auto");
    }
    valueOf() {
      throw new TypeError("use compare() or equals() to compare Temporal.PlainDate");
    }
  }

  function addToDate(o, like, options, sign) {
    const s = slots(o, DATE);
    let r = toDurationRecord(like);
    if (sign < 0) r = negateRecord(r);
    const dd = dateDurationOf(r);
    const overflow = overflowOpt(getOptionsObject(options));
    return makeDate(dateAdd([s[0], s[1], s[2]], dd, overflow), s[3]);
  }

  function diffDates(since, o, other, options) {
    const s = slots(o, DATE);
    const a = [s[0], s[1], s[2]];
    const b = toDate(other);
    if (b[1] !== s[3]) throw new RangeError("the calendars differ");
    const opts = getOptionsObject(options);
    const st = differenceSettings(since, opts, "date", [], "day", "day");
    if (compareDate(a, b[0]) === 0) return makeDuration([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let d = { date: dateUntil(a, b[0], st.largest), time: 0n };
    if (st.smallest !== "day" || st.inc !== 1) {
      const dest = utcEpochNs(b[0][0], b[0][1], b[0][2], 0, 0, 0, 0, 0, 0);
      d = roundRelative(d, dest, a, [0, 0, 0, 0, 0, 0], undefined, st.largest, st.inc, st.smallest, st.mode);
    }
    let r = fromInternal(d, "day");
    if (since) r = negateRecord(r);
    return makeDuration(r);
  }

  // ---- PlainDateTime ----

  function makeDateTime(a, cal, proto) {
    checkDateTime(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8]);
    return create(DATETIME, [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], cal], proto);
  }
  const dtOf = (o) => {
    const s = slots(o, DATETIME);
    return [s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7], s[8]];
  };

  // InterpretTemporalDateTimeFields.
  function dateTimeFromFields(f, overflow) {
    const d = dateFromFields(f, overflow);
    const t = timeFromFields(f, overflow);
    return dtArr([d.year, d.month, d.day], t);
  }

  // ToTemporalDateTime: [array, calendar].
  function toDateTime(item, options) {
    if (isObject(item)) {
      const b = brand(item);
      if (b === DATETIME || b === DATE || b === ZONED) {
        overflowOpt(getOptionsObject(options));
        if (b === DATETIME) {
          const s = slots(item, DATETIME);
          return [dtOf(item), s[9]];
        }
        if (b === DATE) {
          const s = slots(item, DATE);
          return [dtArr([s[0], s[1], s[2]], [0, 0, 0, 0, 0, 0]), s[3]];
        }
        const z = slots(item, ZONED);
        return [zonedDateTime(z[1], z[0]), z[2]];
      }
      const cal = bagCalendar(item);
      const f = prepareFields(item, DATETIME_FIELDS, []);
      const overflow = overflowOpt(getOptionsObject(options));
      const a = dateTimeFromFields(f, overflow);
      checkDateTime(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8]);
      return [a, cal];
    }
    if (typeof item !== "string") throw new TypeError("a date-time must be a string or an object");
    const r = parseDateTime(item);
    if (r.z) throw new RangeError("a UTC designator isn't allowed for a plain date-time");
    const cal = r.calendar === undefined ? "iso8601" : canonCalendar(r.calendar);
    overflowOpt(getOptionsObject(options));
    const a = dtArr(r.date, r.time === undefined ? [0, 0, 0, 0, 0, 0] : r.time);
    checkDateTime(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8]);
    return [a, cal];
  }

  function concatList(a, b) {
    const out = [];
    for (let i = 0; i < a.length; i++) out[out.length] = a[i];
    for (let i = 0; i < b.length; i++) out[out.length] = b[i];
    return out;
  }
  const DATETIME_FIELDS = concatList(DATE_FIELDS, TIME_FIELDS);

  function dateTimeToString(a, cal, precision, show) {
    return `${dateToString(dtDate(a), "iso8601", "never")}T${timeToString(dtTime(a), precision)}${calendarAnnotation(cal, show)}`;
  }

  // RoundISODateTime.
  function roundDateTime(a, inc, unit, mode) {
    const r = roundTime(dtTime(a), inc, unit, mode);
    const d = addDays(dtDate(a), r[0]);
    return [d[0], d[1], d[2], r[1], r[2], r[3], r[4], r[5], r[6]];
  }

  class PlainDateTime {
    constructor(isoYear, isoMonth, isoDay, hour = undefined, minute = undefined, second = undefined,
      millisecond = undefined, microsecond = undefined, nanosecond = undefined, calendar = undefined) {
      const y = toIntegerT(isoYear);
      const m = toIntegerT(isoMonth);
      const d = toIntegerT(isoDay);
      const v = (x) => (x === undefined ? 0 : toIntegerT(x));
      const t = [v(hour), v(minute), v(second), v(millisecond), v(microsecond), v(nanosecond)];
      let cal = "iso8601";
      if (calendar !== undefined) {
        if (typeof calendar !== "string") throw new TypeError("calendar must be a string");
        cal = canonCalendar(calendar);
      }
      if (!isValidISODate(y, m, d)) throw new RangeError("the date is out of range");
      if (!isValidTime(t[0], t[1], t[2], t[3], t[4], t[5])) throw new RangeError("the time is out of range");
      checkDateTime(y, m, d, t[0], t[1], t[2], t[3], t[4], t[5]);
      I.setSlots(this, DATETIME, [y, m, d, t[0], t[1], t[2], t[3], t[4], t[5], cal]);
    }
    static from(item, options = undefined) {
      const r = toDateTime(item, options);
      return makeDateTime(r[0], r[1]);
    }
    static compare(one, two) {
      return compareDT(toDateTime(one)[0], toDateTime(two)[0]);
    }
    get calendarId() { return slots(this, DATETIME)[9]; }
    get era() { slots(this, DATETIME); return undefined; }
    get eraYear() { slots(this, DATETIME); return undefined; }
    get year() { return slots(this, DATETIME)[0]; }
    get month() { return slots(this, DATETIME)[1]; }
    get monthCode() { return monthCode(slots(this, DATETIME)[1]); }
    get day() { return slots(this, DATETIME)[2]; }
    get hour() { return slots(this, DATETIME)[3]; }
    get minute() { return slots(this, DATETIME)[4]; }
    get second() { return slots(this, DATETIME)[5]; }
    get millisecond() { return slots(this, DATETIME)[6]; }
    get microsecond() { return slots(this, DATETIME)[7]; }
    get nanosecond() { return slots(this, DATETIME)[8]; }
    get dayOfWeek() { const a = dtOf(this); return dayOfWeek(a[0], a[1], a[2]); }
    get dayOfYear() { const a = dtOf(this); return dayOfYear(a[0], a[1], a[2]); }
    get weekOfYear() { const a = dtOf(this); return weekOfYear(a[0], a[1], a[2]).week; }
    get yearOfWeek() { const a = dtOf(this); return weekOfYear(a[0], a[1], a[2]).year; }
    get daysInWeek() { slots(this, DATETIME); return 7; }
    get daysInMonth() { const a = dtOf(this); return daysInMonth(a[0], a[1]); }
    get daysInYear() { return daysInYear(dtOf(this)[0]); }
    get monthsInYear() { slots(this, DATETIME); return 12; }
    get inLeapYear() { return isLeap(dtOf(this)[0]); }
    with(temporalDateTimeLike, options = undefined) {
      const s = slots(this, DATETIME);
      checkPartial(temporalDateTimeLike);
      const base = dateFields([s[0], s[1], s[2]]);
      base.hour = s[3]; base.minute = s[4]; base.second = s[5];
      base.millisecond = s[6]; base.microsecond = s[7]; base.nanosecond = s[8];
      const p = prepareFields(temporalDateTimeLike, DATETIME_FIELDS, "partial");
      const f = mergeFields(base, p);
      const overflow = overflowOpt(getOptionsObject(options));
      return makeDateTime(dateTimeFromFields(f, overflow), s[9]);
    }
    withPlainTime(plainTimeLike = undefined) {
      const s = slots(this, DATETIME);
      return makeDateTime(dtArr([s[0], s[1], s[2]], timeOrMidnight(plainTimeLike)), s[9]);
    }
    withCalendar(calendarLike) {
      const s = slots(this, DATETIME);
      return makeDateTime(dtOf(this), toCalendar(calendarLike));
    }
    add(temporalDurationLike, options = undefined) {
      return addToDateTime(this, temporalDurationLike, options, 1);
    }
    subtract(temporalDurationLike, options = undefined) {
      return addToDateTime(this, temporalDurationLike, options, -1);
    }
    until(other, options = undefined) {
      return diffDateTimes(false, this, other, options);
    }
    since(other, options = undefined) {
      return diffDateTimes(true, this, other, options);
    }
    round(roundTo) {
      const s = slots(this, DATETIME);
      const a = dtOf(this);
      const o = roundToOptions(roundTo);
      const inc = roundingIncrementOpt(o);
      const mode = roundingModeOpt(o, "halfExpand");
      const unit = unitOpt(o, "smallestUnit", "time", "required", ["day"]);
      if (unit === "day") validateIncrement(inc, 1, true);
      else validateIncrement(inc, MAXIMUM[unit], false);
      if (unit === "nanosecond" && inc === 1) return makeDateTime(a, s[9]);
      return makeDateTime(roundDateTime(a, inc, unit, mode), s[9]);
    }
    equals(other) {
      const s = slots(this, DATETIME);
      const o = toDateTime(other);
      return compareDT(dtOf(this), o[0]) === 0 && s[9] === o[1];
    }
    toString(options = undefined) {
      const s = slots(this, DATETIME);
      const a = dtOf(this);
      const opts = getOptionsObject(options);
      const show = calendarNameOpt(opts);
      const digits = fractionalDigitsOpt(opts);
      const mode = roundingModeOpt(opts, "trunc");
      const smallest = unitOpt(opts, "smallestUnit", "time", undefined);
      if (smallest === "hour") throw new RangeError("smallestUnit can't be hour here");
      const pr = precisionRecord(smallest, digits);
      const r = roundDateTime(a, pr.increment, pr.unit, mode);
      checkDateTime(r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], r[8]);
      return dateTimeToString(r, s[9], pr.precision, show);
    }
    toLocaleString(locales = undefined, options = undefined) {
      const s = slots(this, DATETIME);
      return dateTimeToString(dtOf(this), s[9], "auto", "auto");
    }
    toJSON() {
      const s = slots(this, DATETIME);
      return dateTimeToString(dtOf(this), s[9], "auto", "auto");
    }
    valueOf() {
      throw new TypeError("use compare() or equals() to compare Temporal.PlainDateTime");
    }
    toZonedDateTime(temporalTimeZoneLike, options = undefined) {
      const s = slots(this, DATETIME);
      const tz = toTimeZone(temporalTimeZoneLike);
      disambiguationOpt(getOptionsObject(options));
      const a = dtOf(this);
      return makeZoned(zonedEpochNs(tz, a[0], a[1], a[2], dtTime(a)), tz, s[9]);
    }
    toPlainDate() {
      const s = slots(this, DATETIME);
      return makeDate([s[0], s[1], s[2]], s[9]);
    }
    toPlainTime() {
      return makeTime(dtTime(dtOf(this)));
    }
  }

  function addToDateTime(o, like, options, sign) {
    const s = slots(o, DATETIME);
    let r = toDurationRecord(like);
    if (sign < 0) r = negateRecord(r);
    const overflow = overflowOpt(getOptionsObject(options));
    const d = internal24(r);
    const a = dtOf(o);
    const tr = balanceTimeNs(timeNs(dtTime(a)) + d.time);
    const date = dateAdd(dtDate(a), [d.date[0], d.date[1], d.date[2], tr[0]], overflow);
    return makeDateTime(dtArr(date, [tr[1], tr[2], tr[3], tr[4], tr[5], tr[6]]), s[9]);
  }

  // DifferencePlainDateTimeWithRounding.
  function diffDateTimeRounded(a, b, largest, inc, smallest, mode) {
    if (compareDT(a, b) === 0) return zeroInternal();
    const d = diffDateTime(dtDate(a), dtTime(a), dtDate(b), dtTime(b), largest);
    if (smallest === "nanosecond" && inc === 1) return d;
    const dest = utcEpochNs(b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8]);
    return roundRelative(d, dest, dtDate(a), dtTime(a), undefined, largest, inc, smallest, mode);
  }

  function diffDateTimes(since, o, other, options) {
    const s = slots(o, DATETIME);
    const a = dtOf(o);
    const b = toDateTime(other);
    if (b[1] !== s[9]) throw new RangeError("the calendars differ");
    const opts = getOptionsObject(options);
    const st = differenceSettings(since, opts, "datetime", [], "nanosecond", "day");
    if (compareDT(a, b[0]) === 0) return makeDuration([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    const d = diffDateTimeRounded(a, b[0], st.largest, st.inc, st.smallest, st.mode);
    let r = fromInternal(d, st.largest);
    if (since) r = negateRecord(r);
    return makeDuration(r);
  }

  // ---- Duration ----

  const durOf = (o) => {
    const s = slots(o, DURATION);
    const r = [];
    for (let i = 0; i < 10; i++) r[i] = s[i];
    return r;
  };

  // GetTemporalRelativeToOption: { plain: [date, cal] } | { zoned: [ns, tz, cal] } | {}.
  function relativeToOpt(o) {
    const v = o.relativeTo;
    if (v === undefined) return {};
    let offsetBehaviour = "option";
    let match = "exactly";
    let cal, f, date, time, tz, offset;
    if (isObject(v)) {
      const b = brand(v);
      if (b === ZONED) {
        const z = slots(v, ZONED);
        return { zoned: [z[0], z[1], z[2]] };
      }
      if (b === DATE) {
        const s = slots(v, DATE);
        return { plain: [[s[0], s[1], s[2]], s[3]] };
      }
      if (b === DATETIME) {
        const s = slots(v, DATETIME);
        return { plain: [[s[0], s[1], s[2]], s[9]] };
      }
      cal = bagCalendar(v);
      f = prepareFields(v, concatList(DATETIME_FIELDS, ["offset", "timeZone"]), []);
      const a = dateTimeFromFields(f, "constrain");
      date = dtDate(a);
      time = dtTime(a);
      tz = f.timeZone;
      offset = f.offset;
      if (offset === undefined) offsetBehaviour = "wall";
    } else {
      if (typeof v !== "string") throw new TypeError("relativeTo must be a string or an object");
      const r = parseDateTime(v);
      if (r.tz === undefined && r.z) fail(v);
      cal = r.calendar === undefined ? "iso8601" : canonCalendar(r.calendar);
      date = r.date;
      time = r.time;
      if (r.tz !== undefined) {
        tz = r.tzOffset ? canonOffsetId(r.tz) : namedZone(r.tz);
        if (r.z) offsetBehaviour = "exact";
        else if (r.offset === undefined) offsetBehaviour = "wall";
        else offset = r.offset;
        match = "minutes";
      }
    }
    if (tz === undefined) {
      checkDate(date[0], date[1], date[2]);
      return { plain: [date, cal] };
    }
    let offNs = 0;
    if (offsetBehaviour === "option") offNs = typeof offset === "string" ? parseOffsetNs(offset) : offset.ns;
    const ns = interpretOffset(date, time, offsetBehaviour, offNs, tz, "compatible", "reject", match);
    return { zoned: [ns, tz, cal] };
  }

  function parseOffsetNs(s) {
    const p = scanner(s);
    return utcOffset(p, false).ns;
  }

  // InterpretISODateTimeOffset (fixed-offset zones have one candidate).
  function interpretOffset(date, time, behaviour, offNs, tz, disambiguation, offsetOption, match) {
    if (time === undefined) return zonedEpochNs(tz, date[0], date[1], date[2], [0, 0, 0, 0, 0, 0]);
    if (behaviour === "wall" || offsetOption === "ignore") return zonedEpochNs(tz, date[0], date[1], date[2], time);
    if (behaviour === "exact" || offsetOption === "use") {
      checkDaysRange(date);
      const e = utcEpochNs(date[0], date[1], date[2], time[0], time[1], time[2], time[3], time[4], time[5]) - BigIntF(offNs);
      checkEpochNs(e);
      return e;
    }
    checkDaysRange(date);
    const utc = utcEpochNs(date[0], date[1], date[2], time[0], time[1], time[2], time[3], time[4], time[5]);
    const cand = zonedEpochNs(tz, date[0], date[1], date[2], time);
    const candOff = NumberF(utc - cand);
    if (candOff === offNs) return cand;
    if (match === "minutes" && NumberF(roundBig(BigIntF(candOff), 60000000000n, "halfExpand")) === offNs) return cand;
    if (offsetOption === "reject") throw new RangeError("the offset doesn't match the time zone");
    return cand;
  }

  // CheckISODaysRange.
  function checkDaysRange(d) {
    if (abs(epochDays(d[0], d[1], d[2])) > 100000000) throw new RangeError("the date is out of range");
  }

  // AddZonedDateTime with an internal duration.
  function addZoned(ns, tz, d, overflow) {
    if (d.date[0] === 0 && d.date[1] === 0 && d.date[2] === 0 && d.date[3] === 0) return addInstant(ns, d.time);
    const a = zonedDateTime(tz, ns);
    const added = dateAdd(dtDate(a), d.date, overflow);
    const t = dtTime(a);
    checkDateTime(added[0], added[1], added[2], t[0], t[1], t[2], t[3], t[4], t[5]);
    return addInstant(zonedEpochNs(tz, added[0], added[1], added[2], t), d.time);
  }

  function addInstant(ns, t) {
    const r = ns + t;
    checkEpochNs(r);
    return r;
  }

  // DifferenceZonedDateTime.
  function diffZoned(ns1, ns2, tz, largest) {
    if (ns1 === ns2) return zeroInternal();
    const s = zonedDateTime(tz, ns1);
    const e = zonedDateTime(tz, ns2);
    if (compareDate(dtDate(s), dtDate(e)) === 0) return { date: [0, 0, 0, 0], time: ns2 - ns1 };
    const sign = ns2 - ns1 < 0n ? -1 : 1;
    const maxCorr = sign === 1 ? 2 : 1;
    let corr = 0;
    let td = timeNs(dtTime(e)) - timeNs(dtTime(s));
    if (bsign(td) === -sign) corr++;
    let ok = false;
    let mid;
    while (corr <= maxCorr && !ok) {
      mid = addDays(dtDate(e), corr * -sign);
      const midNs = zonedEpochNs(tz, mid[0], mid[1], mid[2], dtTime(s));
      td = ns2 - midNs;
      if (sign !== -bsign(td)) ok = true;
      corr++;
    }
    const dd = dateUntil(dtDate(s), mid, largerUnit(largest, "day"));
    return { date: dd, time: td };
  }

  function roundTimeDuration(t, inc, unit, mode) {
    const r = roundBig(t, unitNs(unit) * BigIntF(inc), mode);
    if (babs(r) >= MAX_TIME) throw new RangeError("the duration is out of range");
    return r;
  }

  // DifferenceZonedDateTimeWithRounding.
  function diffZonedRounded(ns1, ns2, tz, largest, inc, smallest, mode) {
    if (unitIndex(largest) > 3) return { date: [0, 0, 0, 0], time: roundTimeDuration(ns2 - ns1, inc, smallest, mode) };
    const d = diffZoned(ns1, ns2, tz, largest);
    if (smallest === "nanosecond" && inc === 1) return d;
    const a = zonedDateTime(tz, ns1);
    return roundRelative(d, ns2, dtDate(a), dtTime(a), tz, largest, inc, smallest, mode);
  }

  function diffZonedTotal(ns1, ns2, tz, unit) {
    if (unitIndex(unit) > 3) return totalTime(ns2 - ns1, unit);
    const d = diffZoned(ns1, ns2, tz, unit);
    const a = zonedDateTime(tz, ns1);
    return totalRelative(d, ns2, dtDate(a), dtTime(a), tz, unit);
  }

  // The plain-relative target of a duration: [start date-time, target date-time].
  function plainTarget(r, plain) {
    const d = internal24(r);
    const tr = balanceTimeNs(d.time);
    const target = dateAdd(plain[0], [d.date[0], d.date[1], d.date[2], tr[0]], "constrain");
    return [dtArr(plain[0], [0, 0, 0, 0, 0, 0]), dtArr(target, [tr[1], tr[2], tr[3], tr[4], tr[5], tr[6]])];
  }

  // DateDurationDays.
  function dateDurationDays(date, plain) {
    if (date[0] === 0 && date[1] === 0 && date[2] === 0) return date[3];
    const later = dateAdd(plain[0], [date[0], date[1], date[2], 0], "constrain");
    return date[3] + epochDays(later[0], later[1], later[2]) - epochDays(plain[0][0], plain[0][1], plain[0][2]);
  }

  function durationToString(r, precision) {
    const sign = durationSign(r);
    const big = (x) => `${BigIntF(abs(x))}`;
    let date = "";
    if (r[0] !== 0) date += `${big(r[0])}Y`;
    if (r[1] !== 0) date += `${big(r[1])}M`;
    if (r[2] !== 0) date += `${big(r[2])}W`;
    if (r[3] !== 0) date += `${big(r[3])}D`;
    let time = "";
    if (r[4] !== 0) time += `${big(r[4])}H`;
    if (r[5] !== 0) time += `${big(r[5])}M`;
    const zeroAbove = r[0] === 0 && r[1] === 0 && r[2] === 0 && r[3] === 0 && r[4] === 0 && r[5] === 0;
    const secs = timeDuration(0, 0, r[6], r[7], r[8], r[9]);
    if (secs !== 0n || zeroAbove || precision !== "auto") {
      const a = babs(secs);
      time += `${a / 1000000000n}${fractional(NumberF(a % 1000000000n), precision)}S`;
    }
    let out = `${sign < 0 ? "-" : ""}P${date}`;
    if (time !== "") out += `T${time}`;
    return out;
  }

  class Duration {
    constructor(years = undefined, months = undefined, weeks = undefined, days = undefined, hours = undefined,
      minutes = undefined, seconds = undefined, milliseconds = undefined, microseconds = undefined,
      nanoseconds = undefined) {
      const v = (x) => (x === undefined ? 0 : toIntegerIfIntegral(x));
      const r = [v(years), v(months), v(weeks), v(days), v(hours), v(minutes), v(seconds), v(milliseconds),
        v(microseconds), v(nanoseconds)];
      if (!isValidDuration(r)) throw new RangeError("the duration is out of range");
      I.setSlots(this, DURATION, r);
    }
    static from(item) {
      return makeDuration(toDurationRecord(item));
    }
    static compare(one, two, options = undefined) {
      const a = toDurationRecord(one);
      const b = toDurationRecord(two);
      const o = getOptionsObject(options);
      const rel = relativeToOpt(o);
      let same = true;
      for (let i = 0; i < 10; i++) if (a[i] !== b[i]) same = false;
      if (same) return 0;
      const l1 = defaultLargest(a);
      const l2 = defaultLargest(b);
      const d1 = internal(a);
      const d2 = internal(b);
      if (rel.zoned !== undefined && (unitIndex(l1) <= 3 || unitIndex(l2) <= 3)) {
        const z = rel.zoned;
        const x = addZoned(z[0], z[1], d1, "constrain");
        const y = addZoned(z[0], z[1], d2, "constrain");
        return x < y ? -1 : x > y ? 1 : 0;
      }
      let days1, days2;
      if (unitIndex(l1) <= 2 || unitIndex(l2) <= 2) {
        if (rel.plain === undefined) throw new RangeError("relativeTo is required to compare calendar units");
        days1 = dateDurationDays(d1.date, rel.plain);
        days2 = dateDurationDays(d2.date, rel.plain);
      } else {
        days1 = a[3];
        days2 = b[3];
      }
      const t1 = d1.time + BigIntF(days1) * NS_PER_DAY;
      const t2 = d2.time + BigIntF(days2) * NS_PER_DAY;
      return t1 < t2 ? -1 : t1 > t2 ? 1 : 0;
    }
    get years() { return slots(this, DURATION)[0]; }
    get months() { return slots(this, DURATION)[1]; }
    get weeks() { return slots(this, DURATION)[2]; }
    get days() { return slots(this, DURATION)[3]; }
    get hours() { return slots(this, DURATION)[4]; }
    get minutes() { return slots(this, DURATION)[5]; }
    get seconds() { return slots(this, DURATION)[6]; }
    get milliseconds() { return slots(this, DURATION)[7]; }
    get microseconds() { return slots(this, DURATION)[8]; }
    get nanoseconds() { return slots(this, DURATION)[9]; }
    get sign() { return durationSign(durOf(this)); }
    get blank() { return durationSign(durOf(this)) === 0; }
    with(temporalDurationLike) {
      const r = durOf(this);
      const p = partialDuration(temporalDurationLike);
      for (let i = 0; i < 10; i++) if (p[i] !== undefined) r[i] = p[i];
      return makeDuration(r);
    }
    negated() {
      return makeDuration(negateRecord(durOf(this)));
    }
    abs() {
      const r = durOf(this);
      for (let i = 0; i < 10; i++) r[i] = abs(r[i]);
      return makeDuration(r);
    }
    add(other) {
      return addDurations(this, other, 1);
    }
    subtract(other) {
      return addDurations(this, other, -1);
    }
    round(roundTo) {
      const r = durOf(this);
      const o = roundToOptions(roundTo);
      let largest = unitOpt(o, "largestUnit", "datetime", undefined, ["auto"]);
      const rel = relativeToOpt(o);
      const inc = roundingIncrementOpt(o);
      const mode = roundingModeOpt(o, "halfExpand");
      let smallest = unitOpt(o, "smallestUnit", "datetime", undefined);
      let smallestPresent = true;
      if (smallest === undefined) {
        smallestPresent = false;
        smallest = "nanosecond";
      }
      const existing = defaultLargest(r);
      const defLargest = largerUnit(existing, smallest);
      let largestPresent = true;
      if (largest === undefined) {
        largestPresent = false;
        largest = defLargest;
      } else if (largest === "auto") largest = defLargest;
      if (!smallestPresent && !largestPresent) throw new RangeError("smallestUnit or largestUnit is required");
      if (largerUnit(largest, smallest) !== largest) throw new RangeError("largestUnit is smaller than smallestUnit");
      const max = MAXIMUM[smallest];
      if (max !== undefined) validateIncrement(inc, max, false);
      if (inc > 1 && largest !== smallest && unitIndex(smallest) <= 3) {
        throw new RangeError("roundingIncrement > 1 needs largestUnit equal to a date smallestUnit");
      }
      if (rel.zoned !== undefined) {
        const z = rel.zoned;
        const d = internal(r);
        const target = addZoned(z[0], z[1], d, "constrain");
        const res = diffZonedRounded(z[0], target, z[1], largest, inc, smallest, mode);
        if (unitIndex(largest) <= 3) largest = "hour";
        return makeDuration(fromInternal(res, largest));
      }
      if (rel.plain !== undefined) {
        const tg = plainTarget(r, rel.plain);
        const res = diffDateTimeRounded(tg[0], tg[1], largest, inc, smallest, mode);
        return makeDuration(fromInternal(res, largest));
      }
      if (unitIndex(existing) <= 2 || unitIndex(largest) <= 2) {
        throw new RangeError("relativeTo is required to round calendar units");
      }
      const d = internal24(r);
      let res;
      if (smallest === "day") {
        const days = roundBig(d.time, NS_PER_DAY * BigIntF(inc), mode) / NS_PER_DAY;
        res = { date: [0, 0, 0, NumberF(days)], time: 0n };
      } else {
        res = { date: [0, 0, 0, 0], time: roundTimeDuration(d.time, inc, smallest, mode) };
      }
      return makeDuration(fromInternal(res, largest));
    }
    total(totalOf) {
      const r = durOf(this);
      if (totalOf === undefined) throw new TypeError("options are required");
      let o;
      if (typeof totalOf === "string") {
        o = rec();
        I.defineValue(o, "unit", totalOf, true, true, true);
      } else o = getOptionsObject(totalOf);
      const rel = relativeToOpt(o);
      const unit = unitOpt(o, "unit", "datetime", "required");
      if (rel.zoned !== undefined) {
        const z = rel.zoned;
        const target = addZoned(z[0], z[1], internal(r), "constrain");
        return diffZonedTotal(z[0], target, z[1], unit);
      }
      if (rel.plain !== undefined) {
        const tg = plainTarget(r, rel.plain);
        const a = tg[0], b = tg[1];
        if (compareDT(a, b) === 0) return 0;
        const d = diffDateTime(dtDate(a), dtTime(a), dtDate(b), dtTime(b), unit);
        if (unit === "nanosecond") return NumberF(d.time);
        const dest = utcEpochNs(b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8]);
        return totalRelative(d, dest, dtDate(a), dtTime(a), undefined, unit);
      }
      const largest = defaultLargest(r);
      if (unitIndex(largest) <= 2 || unitIndex(unit) <= 2) {
        throw new RangeError("relativeTo is required for calendar units");
      }
      return totalTime(internal24(r).time, unit);
    }
    toString(options = undefined) {
      const r = durOf(this);
      const o = getOptionsObject(options);
      const digits = fractionalDigitsOpt(o);
      const mode = roundingModeOpt(o, "trunc");
      const smallest = unitOpt(o, "smallestUnit", "time", undefined);
      if (smallest === "hour" || smallest === "minute") throw new RangeError("smallestUnit must be seconds or smaller");
      const pr = precisionRecord(smallest, digits);
      if (pr.unit === "nanosecond" && pr.increment === 1) return durationToString(r, pr.precision);
      const largest = defaultLargest(r);
      const d = internal(r);
      const t = roundTimeDuration(d.time, pr.increment, pr.unit, mode);
      const rounded = fromInternal({ date: d.date, time: t }, largerUnit(largest, "second"));
      if (!isValidDuration(rounded)) throw new RangeError("the duration is out of range");
      return durationToString(rounded, pr.precision);
    }
    toJSON() {
      return durationToString(durOf(this), "auto");
    }
    toLocaleString(locales = undefined, options = undefined) {
      return durationToString(durOf(this), "auto");
    }
    valueOf() {
      throw new TypeError("use compare() to compare Temporal.Duration");
    }
  }

  function addDurations(o, other, sign) {
    const a = durOf(o);
    let b = toDurationRecord(other);
    if (sign < 0) b = negateRecord(b);
    const largest = largerUnit(defaultLargest(a), defaultLargest(b));
    if (unitIndex(largest) <= 2) throw new RangeError("calendar units can't be added without relativeTo");
    const t = internal24(a).time + internal24(b).time;
    if (babs(t) >= MAX_TIME) throw new RangeError("the duration is out of range");
    return makeDuration(fromInternal({ date: [0, 0, 0, 0], time: t }, largest));
  }

  // ---- Instant ----

  function makeInstant(ns, proto) {
    checkEpochNs(ns);
    return create(INSTANT, [ns], proto);
  }

  function toBigIntValue(v) {
    const p = isObject(v) ? I.toPrimitiveValue(v, "number") : v;
    if (typeof p === "number") throw new TypeError("a BigInt is required");
    return BigIntF(p);
  }

  // ToTemporalInstant: epoch nanoseconds.
  function toInstantNs(item) {
    let v = item;
    if (isObject(v)) {
      const b = brand(v);
      if (b === INSTANT) return slots(v, INSTANT)[0];
      if (b === ZONED) return slots(v, ZONED)[0];
      v = I.toPrimitiveValue(v, "string");
    }
    if (typeof v !== "string") throw new TypeError("an instant must be a string or an object");
    const r = parseDateTime(v);
    if (r.time === undefined || (!r.z && r.offset === undefined)) throw new RangeError("an instant needs a UTC offset");
    if (r.calendar !== undefined) canonCalendar(r.calendar);
    const off = r.z ? 0 : r.offset.ns;
    const t = r.time;
    const local = utcEpochNs(r.date[0], r.date[1], r.date[2], t[0], t[1], t[2], t[3], t[4], t[5]);
    checkDaysRange(r.date);
    const ns = local - BigIntF(off);
    checkEpochNs(ns);
    return ns;
  }

  const INSTANT_MAX = { hour: 24, minute: 1440, second: 86400, millisecond: 8.64e7, microsecond: 8.64e10,
    nanosecond: 8.64e13 };

  function instantToString(ns, tz, precision) {
    const out = tz === undefined ? "UTC" : tz;
    const a = zonedDateTime(out, ns);
    const s = dateTimeToString(a, "iso8601", precision, "never");
    if (tz === undefined) return `${s}Z`;
    return `${s}${formatOffsetRounded(zoneOffset(out))}`;
  }

  // FormatDateTimeUTCOffsetRounded.
  function formatOffsetRounded(ns) {
    const r = NumberF(roundBig(BigIntF(ns), 60000000000n, "halfExpand"));
    return formatOffsetNs(r, true);
  }

  class Instant {
    constructor(epochNanoseconds) {
      const ns = toBigIntValue(epochNanoseconds);
      checkEpochNs(ns);
      I.setSlots(this, INSTANT, [ns]);
    }
    static from(item) {
      return makeInstant(toInstantNs(item));
    }
    static fromEpochMilliseconds(epochMilliseconds) {
      const n = +epochMilliseconds;
      if (!isFiniteN(n) || trunc(n) !== n) throw new RangeError("epochMilliseconds must be an integer");
      return makeInstant(BigIntF(n) * 1000000n);
    }
    static fromEpochNanoseconds(epochNanoseconds) {
      return makeInstant(toBigIntValue(epochNanoseconds));
    }
    static compare(one, two) {
      const a = toInstantNs(one);
      const b = toInstantNs(two);
      return a < b ? -1 : a > b ? 1 : 0;
    }
    get epochMilliseconds() { return NumberF(bdiv(slots(this, INSTANT)[0], 1000000n)); }
    get epochNanoseconds() { return slots(this, INSTANT)[0]; }
    add(temporalDurationLike) {
      return addToInstant(this, temporalDurationLike, 1);
    }
    subtract(temporalDurationLike) {
      return addToInstant(this, temporalDurationLike, -1);
    }
    until(other, options = undefined) {
      return diffInstants(false, this, other, options);
    }
    since(other, options = undefined) {
      return diffInstants(true, this, other, options);
    }
    round(roundTo) {
      const ns = slots(this, INSTANT)[0];
      const o = roundToOptions(roundTo);
      const inc = roundingIncrementOpt(o);
      const mode = roundingModeOpt(o, "halfExpand");
      const unit = unitOpt(o, "smallestUnit", "time", "required");
      validateIncrement(inc, INSTANT_MAX[unit], true);
      return makeInstant(roundBig(ns, unitNs(unit) * BigIntF(inc), mode));
    }
    equals(other) {
      const ns = slots(this, INSTANT)[0];
      return ns === toInstantNs(other);
    }
    toString(options = undefined) {
      const ns = slots(this, INSTANT)[0];
      const o = getOptionsObject(options);
      const digits = fractionalDigitsOpt(o);
      const mode = roundingModeOpt(o, "trunc");
      const smallest = unitOpt(o, "smallestUnit", "time", undefined);
      let tz = o.timeZone;
      if (smallest === "hour") throw new RangeError("smallestUnit can't be hour here");
      if (tz !== undefined) tz = toTimeZone(tz);
      const pr = precisionRecord(smallest, digits);
      const rounded = roundBig(ns, unitNs(pr.unit) * BigIntF(pr.increment), mode);
      checkEpochNs(rounded);
      return instantToString(rounded, tz, pr.precision);
    }
    toJSON() {
      return instantToString(slots(this, INSTANT)[0], undefined, "auto");
    }
    toLocaleString(locales = undefined, options = undefined) {
      return instantToString(slots(this, INSTANT)[0], undefined, "auto");
    }
    valueOf() {
      throw new TypeError("use compare() or equals() to compare Temporal.Instant");
    }
    toZonedDateTimeISO(timeZone) {
      const ns = slots(this, INSTANT)[0];
      return makeZoned(ns, toTimeZone(timeZone), "iso8601");
    }
  }

  function addToInstant(o, like, sign) {
    const ns = slots(o, INSTANT)[0];
    let r = toDurationRecord(like);
    if (sign < 0) r = negateRecord(r);
    if (unitIndex(defaultLargest(r)) <= 3) throw new RangeError("an instant can't add days or larger units");
    return makeInstant(addInstant(ns, internal24(r).time));
  }

  function diffInstants(since, o, other, options) {
    const a = slots(o, INSTANT)[0];
    const b = toInstantNs(other);
    const st = differenceSettings(since, getOptionsObject(options), "time", [], "nanosecond", "second");
    const t = roundTimeDuration(b - a, st.inc, st.smallest, st.mode);
    let r = fromInternal({ date: [0, 0, 0, 0], time: t }, st.largest);
    if (since) r = negateRecord(r);
    return makeDuration(r);
  }

  // ---- ZonedDateTime ----

  function makeZoned(ns, tz, cal, proto) {
    checkEpochNs(ns);
    return create(ZONED, [ns, tz, cal], proto);
  }
  const zOf = (o) => {
    const s = slots(o, ZONED);
    return [s[0], s[1], s[2]];
  };
  const zLocal = (o) => {
    const z = zOf(o);
    return zonedDateTime(z[1], z[0]);
  };

  // ParseTimeZoneIdentifier (identifiers only, for the constructor).
  function timeZoneIdentifier(s) {
    if (s.length > 0 && isSign(cu(s, 0))) return canonOffsetId(s);
    if (!isTimeZoneName(s)) throw new RangeError(`invalid time zone ${s}`);
    return namedZone(s);
  }

  // ToTemporalZonedDateTime: [ns, tz, cal].
  function toZoned(item, options) {
    let behaviour = "option";
    let match = "exactly";
    let cal, tz, offset, date, time, disamb, offsetOption;
    if (isObject(item)) {
      if (brand(item) === ZONED) {
        const o = getOptionsObject(options);
        disambiguationOpt(o);
        offsetOpt(o, "reject");
        overflowOpt(o);
        return zOf(item);
      }
      cal = bagCalendar(item);
      const f = prepareFields(item, concatList(DATETIME_FIELDS, ["offset", "timeZone"]), ["timeZone"]);
      tz = f.timeZone;
      offset = f.offset;
      if (offset === undefined) behaviour = "wall";
      const o = getOptionsObject(options);
      disamb = disambiguationOpt(o);
      offsetOption = offsetOpt(o, "reject");
      const overflow = overflowOpt(o);
      const a = dateTimeFromFields(f, overflow);
      date = dtDate(a);
      time = dtTime(a);
    } else {
      if (typeof item !== "string") throw new TypeError("a zoned date-time must be a string or an object");
      const r = parseDateTime(item);
      if (r.tz === undefined) throw new RangeError("a time zone annotation is required");
      tz = r.tzOffset ? canonOffsetId(r.tz) : namedZone(r.tz);
      offset = r.offset;
      if (r.z) behaviour = "exact";
      else if (offset === undefined) behaviour = "wall";
      cal = r.calendar === undefined ? "iso8601" : canonCalendar(r.calendar);
      match = "minutes";
      const o = getOptionsObject(options);
      disamb = disambiguationOpt(o);
      offsetOption = offsetOpt(o, "reject");
      overflowOpt(o);
      date = r.date;
      time = r.time;
    }
    let offNs = 0;
    if (behaviour === "option") offNs = typeof offset === "string" ? parseOffsetNs(offset) : offset.ns;
    const ns = interpretOffset(date, time, behaviour, offNs, tz, disamb, offsetOption, match);
    return [ns, tz, cal];
  }

  function zonedToString(z, precision, showCal, showTz, showOffset, inc, unit, mode) {
    const ns = roundBig(z[0], unitNs(unit) * BigIntF(inc), mode);
    checkEpochNs(ns);
    const a = zonedDateTime(z[1], ns);
    let s = dateTimeToString(a, "iso8601", precision, "never");
    if (showOffset !== "never") s += formatOffsetRounded(zoneOffset(z[1]));
    if (showTz !== "never") s += `[${showTz === "critical" ? "!" : ""}${z[1]}]`;
    return s + calendarAnnotation(z[2], showCal);
  }

  class ZonedDateTime {
    constructor(epochNanoseconds, timeZone, calendar = undefined) {
      const ns = toBigIntValue(epochNanoseconds);
      checkEpochNs(ns);
      if (typeof timeZone !== "string") throw new TypeError("timeZone must be a string");
      const tz = timeZoneIdentifier(timeZone);
      let cal = "iso8601";
      if (calendar !== undefined) {
        if (typeof calendar !== "string") throw new TypeError("calendar must be a string");
        cal = canonCalendar(calendar);
      }
      I.setSlots(this, ZONED, [ns, tz, cal]);
    }
    static from(item, options = undefined) {
      const z = toZoned(item, options);
      return makeZoned(z[0], z[1], z[2]);
    }
    static compare(one, two) {
      const a = toZoned(one)[0];
      const b = toZoned(two)[0];
      return a < b ? -1 : a > b ? 1 : 0;
    }
    get calendarId() { return slots(this, ZONED)[2]; }
    get timeZoneId() { return slots(this, ZONED)[1]; }
    get era() { slots(this, ZONED); return undefined; }
    get eraYear() { slots(this, ZONED); return undefined; }
    get year() { return zLocal(this)[0]; }
    get month() { return zLocal(this)[1]; }
    get monthCode() { return monthCode(zLocal(this)[1]); }
    get day() { return zLocal(this)[2]; }
    get hour() { return zLocal(this)[3]; }
    get minute() { return zLocal(this)[4]; }
    get second() { return zLocal(this)[5]; }
    get millisecond() { return zLocal(this)[6]; }
    get microsecond() { return zLocal(this)[7]; }
    get nanosecond() { return zLocal(this)[8]; }
    get epochMilliseconds() { return NumberF(bdiv(slots(this, ZONED)[0], 1000000n)); }
    get epochNanoseconds() { return slots(this, ZONED)[0]; }
    get dayOfWeek() { const a = zLocal(this); return dayOfWeek(a[0], a[1], a[2]); }
    get dayOfYear() { const a = zLocal(this); return dayOfYear(a[0], a[1], a[2]); }
    get weekOfYear() { const a = zLocal(this); return weekOfYear(a[0], a[1], a[2]).week; }
    get yearOfWeek() { const a = zLocal(this); return weekOfYear(a[0], a[1], a[2]).year; }
    get hoursInDay() {
      const z = zOf(this);
      const a = zonedDateTime(z[1], z[0]);
      const today = dtDate(a);
      const tomorrow = addDays(today, 1);
      const s = zonedEpochNs(z[1], today[0], today[1], today[2], [0, 0, 0, 0, 0, 0]);
      const e = zonedEpochNs(z[1], tomorrow[0], tomorrow[1], tomorrow[2], [0, 0, 0, 0, 0, 0]);
      return totalTime(e - s, "hour");
    }
    get daysInWeek() { slots(this, ZONED); return 7; }
    get daysInMonth() { const a = zLocal(this); return daysInMonth(a[0], a[1]); }
    get daysInYear() { return daysInYear(zLocal(this)[0]); }
    get monthsInYear() { slots(this, ZONED); return 12; }
    get inLeapYear() { return isLeap(zLocal(this)[0]); }
    get offsetNanoseconds() { return zoneOffset(slots(this, ZONED)[1]); }
    get offset() { return formatOffsetNs(zoneOffset(slots(this, ZONED)[1]), false); }
    with(temporalZonedDateTimeLike, options = undefined) {
      const z = zOf(this);
      checkPartial(temporalZonedDateTimeLike);
      const a = zonedDateTime(z[1], z[0]);
      const base = dateFields(dtDate(a));
      base.hour = a[3]; base.minute = a[4]; base.second = a[5];
      base.millisecond = a[6]; base.microsecond = a[7]; base.nanosecond = a[8];
      base.offset = formatOffsetNs(zoneOffset(z[1]), false);
      const p = prepareFields(temporalZonedDateTimeLike, concatList(DATETIME_FIELDS, ["offset"]), "partial");
      const f = mergeFields(base, p);
      const o = getOptionsObject(options);
      const disamb = disambiguationOpt(o);
      const offsetOption = offsetOpt(o, "prefer");
      const overflow = overflowOpt(o);
      const r = dateTimeFromFields(f, overflow);
      const offNs = parseOffsetNs(f.offset);
      const ns = interpretOffset(dtDate(r), dtTime(r), "option", offNs, z[1], disamb, offsetOption, "exactly");
      return makeZoned(ns, z[1], z[2]);
    }
    withPlainTime(plainTimeLike = undefined) {
      const z = zOf(this);
      const d = dtDate(zonedDateTime(z[1], z[0]));
      const t = timeOrMidnight(plainTimeLike);
      return makeZoned(zonedEpochNs(z[1], d[0], d[1], d[2], t), z[1], z[2]);
    }
    withTimeZone(timeZoneLike) {
      const z = zOf(this);
      return makeZoned(z[0], toTimeZone(timeZoneLike), z[2]);
    }
    withCalendar(calendarLike) {
      const z = zOf(this);
      return makeZoned(z[0], z[1], toCalendar(calendarLike));
    }
    add(temporalDurationLike, options = undefined) {
      return addToZoned(this, temporalDurationLike, options, 1);
    }
    subtract(temporalDurationLike, options = undefined) {
      return addToZoned(this, temporalDurationLike, options, -1);
    }
    until(other, options = undefined) {
      return diffZonedTimes(false, this, other, options);
    }
    since(other, options = undefined) {
      return diffZonedTimes(true, this, other, options);
    }
    round(roundTo) {
      const z = zOf(this);
      const o = roundToOptions(roundTo);
      const inc = roundingIncrementOpt(o);
      const mode = roundingModeOpt(o, "halfExpand");
      const unit = unitOpt(o, "smallestUnit", "time", "required", ["day"]);
      if (unit === "day") validateIncrement(inc, 1, true);
      else validateIncrement(inc, MAXIMUM[unit], false);
      if (unit === "nanosecond" && inc === 1) return makeZoned(z[0], z[1], z[2]);
      const a = zonedDateTime(z[1], z[0]);
      let ns;
      if (unit === "day") {
        const ds = dtDate(a);
        const de = addDays(ds, 1);
        const start = zonedEpochNs(z[1], ds[0], ds[1], ds[2], [0, 0, 0, 0, 0, 0]);
        const end = zonedEpochNs(z[1], de[0], de[1], de[2], [0, 0, 0, 0, 0, 0]);
        ns = start + roundBig(z[0] - start, end - start, mode);
      } else {
        const r = roundDateTime(a, inc, unit, mode);
        ns = interpretOffset(dtDate(r), dtTime(r), "option", zoneOffset(z[1]), z[1], "compatible", "prefer", "exactly");
      }
      return makeZoned(ns, z[1], z[2]);
    }
    equals(other) {
      const z = zOf(this);
      const o = toZoned(other);
      return z[0] === o[0] && z[1] === o[1] && z[2] === o[2];
    }
    toString(options = undefined) {
      const z = zOf(this);
      const o = getOptionsObject(options);
      const showCal = calendarNameOpt(o);
      const digits = fractionalDigitsOpt(o);
      const showOffset = showOffsetOpt(o);
      const mode = roundingModeOpt(o, "trunc");
      const smallest = unitOpt(o, "smallestUnit", "time", undefined);
      if (smallest === "hour") throw new RangeError("smallestUnit can't be hour here");
      const showTz = timeZoneNameOpt(o);
      const pr = precisionRecord(smallest, digits);
      return zonedToString(z, pr.precision, showCal, showTz, showOffset, pr.increment, pr.unit, mode);
    }
    toLocaleString(locales = undefined, options = undefined) {
      return zonedToString(zOf(this), "auto", "auto", "auto", "auto", 1, "nanosecond", "trunc");
    }
    toJSON() {
      return zonedToString(zOf(this), "auto", "auto", "auto", "auto", 1, "nanosecond", "trunc");
    }
    valueOf() {
      throw new TypeError("use compare() or equals() to compare Temporal.ZonedDateTime");
    }
    startOfDay() {
      const z = zOf(this);
      const d = dtDate(zonedDateTime(z[1], z[0]));
      return makeZoned(zonedEpochNs(z[1], d[0], d[1], d[2], [0, 0, 0, 0, 0, 0]), z[1], z[2]);
    }
    getTimeZoneTransition(directionParam) {
      slots(this, ZONED);
      if (directionParam === undefined) throw new TypeError("a direction is required");
      let o;
      if (typeof directionParam === "string") {
        o = rec();
        I.defineValue(o, "direction", directionParam, true, true, true);
      } else o = getOptionsObject(directionParam);
      const dir = getStringOption(o, "direction", ["next", "previous"], undefined);
      if (dir === undefined) throw new RangeError("direction is required");
      // Fixed-offset zones and UTC have no transitions.
      return null;
    }
    toInstant() {
      return makeInstant(slots(this, ZONED)[0]);
    }
    toPlainDate() {
      const z = zOf(this);
      return makeDate(dtDate(zonedDateTime(z[1], z[0])), z[2]);
    }
    toPlainTime() {
      return makeTime(dtTime(zLocal(this)));
    }
    toPlainDateTime() {
      const z = zOf(this);
      return makeDateTime(zonedDateTime(z[1], z[0]), z[2]);
    }
  }

  function addToZoned(o, like, options, sign) {
    const z = zOf(o);
    let r = toDurationRecord(like);
    if (sign < 0) r = negateRecord(r);
    const overflow = overflowOpt(getOptionsObject(options));
    return makeZoned(addZoned(z[0], z[1], internal(r), overflow), z[1], z[2]);
  }

  function diffZonedTimes(since, o, other, options) {
    const z = zOf(o);
    const w = toZoned(other);
    if (w[2] !== z[2]) throw new RangeError("the calendars differ");
    const st = differenceSettings(since, getOptionsObject(options), "datetime", [], "nanosecond", "hour");
    let r;
    if (unitIndex(st.largest) > 3) {
      const t = roundTimeDuration(w[0] - z[0], st.inc, st.smallest, st.mode);
      r = fromInternal({ date: [0, 0, 0, 0], time: t }, st.largest);
    } else {
      if (w[1] !== z[1]) throw new RangeError("the time zones differ");
      if (w[0] === z[0]) return makeDuration([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
      const d = diffZonedRounded(z[0], w[0], z[1], st.largest, st.inc, st.smallest, st.mode);
      r = fromInternal(d, "hour");
    }
    if (since) r = negateRecord(r);
    return makeDuration(r);
  }

  // ---- PlainYearMonth ----

  function makeYearMonth(d, cal, proto) {
    checkYearMonth(d[0], d[1]);
    return create(YEARMONTH, [d[0], d[1], d[2], cal], proto);
  }

  // CalendarYearMonthFromFields (ISO).
  function yearMonthFromFields(f, overflow) {
    if (f.year === undefined) throw new TypeError("year is required");
    const m = resolveMonth(f);
    const d = regulateDate(f.year, m, 1, overflow);
    checkYearMonth(d.year, d.month);
    return [d.year, d.month, 1];
  }

  // ToTemporalYearMonth: [date, cal].
  function toYearMonth(item, options) {
    if (isObject(item)) {
      if (brand(item) === YEARMONTH) {
        overflowOpt(getOptionsObject(options));
        const s = slots(item, YEARMONTH);
        return [[s[0], s[1], s[2]], s[3]];
      }
      const cal = bagCalendar(item);
      const f = prepareFields(item, ["month", "monthCode", "year"], []);
      const overflow = overflowOpt(getOptionsObject(options));
      return [yearMonthFromFields(f, overflow), cal];
    }
    if (typeof item !== "string") throw new TypeError("a year-month must be a string or an object");
    const r = parseYearMonthString(item);
    const cal = r.calendar === undefined ? "iso8601" : canonCalendar(r.calendar);
    overflowOpt(getOptionsObject(options));
    checkYearMonth(r.date[0], r.date[1]);
    return [[r.date[0], r.date[1], 1], cal];
  }

  function yearMonthToString(s, show) {
    let out = `${padYear(s[0])}-${pad(s[1], 2)}`;
    if (show === "always" || show === "critical" || s[3] !== "iso8601") out += `-${pad(s[2], 2)}`;
    return out + calendarAnnotation(s[3], show);
  }

  class PlainYearMonth {
    constructor(isoYear, isoMonth, calendar = undefined, referenceISODay = undefined) {
      const y = toIntegerT(isoYear);
      const m = toIntegerT(isoMonth);
      let cal = "iso8601";
      if (calendar !== undefined) {
        if (typeof calendar !== "string") throw new TypeError("calendar must be a string");
        cal = canonCalendar(calendar);
      }
      const ref = referenceISODay === undefined ? 1 : toIntegerT(referenceISODay);
      if (!isValidISODate(y, m, ref)) throw new RangeError("the date is out of range");
      checkYearMonth(y, m);
      I.setSlots(this, YEARMONTH, [y, m, ref, cal]);
    }
    static from(item, options = undefined) {
      const r = toYearMonth(item, options);
      return makeYearMonth(r[0], r[1]);
    }
    static compare(one, two) {
      return compareDate(toYearMonth(one)[0], toYearMonth(two)[0]);
    }
    get calendarId() { return slots(this, YEARMONTH)[3]; }
    get era() { slots(this, YEARMONTH); return undefined; }
    get eraYear() { slots(this, YEARMONTH); return undefined; }
    get year() { return slots(this, YEARMONTH)[0]; }
    get month() { return slots(this, YEARMONTH)[1]; }
    get monthCode() { return monthCode(slots(this, YEARMONTH)[1]); }
    get daysInYear() { return daysInYear(slots(this, YEARMONTH)[0]); }
    get daysInMonth() { const s = slots(this, YEARMONTH); return daysInMonth(s[0], s[1]); }
    get monthsInYear() { slots(this, YEARMONTH); return 12; }
    get inLeapYear() { return isLeap(slots(this, YEARMONTH)[0]); }
    with(temporalYearMonthLike, options = undefined) {
      const s = slots(this, YEARMONTH);
      checkPartial(temporalYearMonthLike);
      const base = rec();
      base.year = s[0];
      base.month = s[1];
      base.monthCode = monthCode(s[1]);
      const p = prepareFields(temporalYearMonthLike, ["month", "monthCode", "year"], "partial");
      const f = mergeFields(base, p);
      const overflow = overflowOpt(getOptionsObject(options));
      return makeYearMonth(yearMonthFromFields(f, overflow), s[3]);
    }
    add(temporalDurationLike, options = undefined) {
      return addToYearMonth(this, temporalDurationLike, options, 1);
    }
    subtract(temporalDurationLike, options = undefined) {
      return addToYearMonth(this, temporalDurationLike, options, -1);
    }
    until(other, options = undefined) {
      return diffYearMonths(false, this, other, options);
    }
    since(other, options = undefined) {
      return diffYearMonths(true, this, other, options);
    }
    equals(other) {
      const s = slots(this, YEARMONTH);
      const o = toYearMonth(other);
      return compareDate([s[0], s[1], s[2]], o[0]) === 0 && s[3] === o[1];
    }
    toString(options = undefined) {
      const s = slots(this, YEARMONTH);
      return yearMonthToString(s, calendarNameOpt(getOptionsObject(options)));
    }
    toLocaleString(locales = undefined, options = undefined) {
      return yearMonthToString(slots(this, YEARMONTH), "auto");
    }
    toJSON() {
      return yearMonthToString(slots(this, YEARMONTH), "auto");
    }
    valueOf() {
      throw new TypeError("use compare() or equals() to compare Temporal.PlainYearMonth");
    }
    toPlainDate(item) {
      const s = slots(this, YEARMONTH);
      if (!isObject(item)) throw new TypeError("an object with a day is required");
      const base = rec();
      base.year = s[0];
      base.month = s[1];
      base.monthCode = monthCode(s[1]);
      const p = prepareFields(item, ["day"], []);
      const f = mergeFields(base, p);
      const d = dateFromFields(f, "constrain");
      return makeDate([d.year, d.month, d.day], s[3]);
    }
  }

  function addToYearMonth(o, like, options, sign) {
    const s = slots(o, YEARMONTH);
    let r = toDurationRecord(like);
    if (sign < 0) r = negateRecord(r);
    const overflow = overflowOpt(getOptionsObject(options));
    const dsign = durationSign(r);
    let date = [s[0], s[1], 1];
    if (dsign < 0) {
      const next = dateAdd(date, [0, 1, 0, 0], "constrain");
      date = addDays(next, -1);
    }
    const dd = dateDurationOf(r);
    const added = dateAdd(date, dd, overflow);
    return makeYearMonth([added[0], added[1], 1], s[3]);
  }

  function diffYearMonths(since, o, other, options) {
    const s = slots(o, YEARMONTH);
    const b = toYearMonth(other);
    if (b[1] !== s[3]) throw new RangeError("the calendars differ");
    const st = differenceSettings(since, getOptionsObject(options), "date", ["week", "day"], "month", "year");
    const a = [s[0], s[1], 1];
    const bb = [b[0][0], b[0][1], 1];
    if (compareDate([s[0], s[1], s[2]], b[0]) === 0) return makeDuration([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    const dd = dateUntil(a, bb, st.largest);
    let d = { date: [dd[0], dd[1], 0, 0], time: 0n };
    if (st.smallest !== "month" || st.inc !== 1) {
      const dest = utcEpochNs(bb[0], bb[1], bb[2], 0, 0, 0, 0, 0, 0);
      d = roundRelative(d, dest, a, [0, 0, 0, 0, 0, 0], undefined, st.largest, st.inc, st.smallest, st.mode);
    }
    let r = fromInternal(d, "day");
    if (since) r = negateRecord(r);
    return makeDuration(r);
  }

  // ---- PlainMonthDay ----

  function makeMonthDay(d, cal, proto) {
    return create(MONTHDAY, [d[0], d[1], d[2], cal], proto);
  }

  // CalendarMonthDayFromFields (ISO): [1972, month, day].
  function monthDayFromFields(f, overflow) {
    if (f.monthCode === undefined && f.month !== undefined && f.year === undefined) {
      throw new TypeError("year or monthCode is required with month");
    }
    const m = resolveMonth(f);
    if (f.day === undefined) throw new TypeError("day is required");
    const y = f.year === undefined ? 1972 : f.year;
    const d = regulateDate(y, m, f.day, overflow);
    return [1972, d.month, d.day];
  }

  // ToTemporalMonthDay: [date, cal].
  function toMonthDay(item, options) {
    if (isObject(item)) {
      if (brand(item) === MONTHDAY) {
        overflowOpt(getOptionsObject(options));
        const s = slots(item, MONTHDAY);
        return [[s[0], s[1], s[2]], s[3]];
      }
      const cal = bagCalendar(item);
      const f = prepareFields(item, DATE_FIELDS, []);
      const overflow = overflowOpt(getOptionsObject(options));
      return [monthDayFromFields(f, overflow), cal];
    }
    if (typeof item !== "string") throw new TypeError("a month-day must be a string or an object");
    const r = parseMonthDayString(item);
    const cal = r.calendar === undefined ? "iso8601" : canonCalendar(r.calendar);
    overflowOpt(getOptionsObject(options));
    if (r.date[0] === undefined) return [[1972, r.date[1], r.date[2]], cal];
    checkDate(r.date[0], r.date[1], r.date[2]);
    return [[1972, r.date[1], r.date[2]], cal];
  }

  function monthDayToString(s, show) {
    let out = `${pad(s[1], 2)}-${pad(s[2], 2)}`;
    if (show === "always" || show === "critical" || s[3] !== "iso8601") out = `${padYear(s[0])}-${out}`;
    return out + calendarAnnotation(s[3], show);
  }

  class PlainMonthDay {
    constructor(isoMonth, isoDay, calendar = undefined, referenceISOYear = undefined) {
      const m = toIntegerT(isoMonth);
      const d = toIntegerT(isoDay);
      let cal = "iso8601";
      if (calendar !== undefined) {
        if (typeof calendar !== "string") throw new TypeError("calendar must be a string");
        cal = canonCalendar(calendar);
      }
      const y = referenceISOYear === undefined ? 1972 : toIntegerT(referenceISOYear);
      if (!isValidISODate(y, m, d)) throw new RangeError("the date is out of range");
      checkDate(y, m, d);
      I.setSlots(this, MONTHDAY, [y, m, d, cal]);
    }
    static from(item, options = undefined) {
      const r = toMonthDay(item, options);
      return makeMonthDay(r[0], r[1]);
    }
    get calendarId() { return slots(this, MONTHDAY)[3]; }
    get monthCode() { return monthCode(slots(this, MONTHDAY)[1]); }
    get day() { return slots(this, MONTHDAY)[2]; }
    with(temporalMonthDayLike, options = undefined) {
      const s = slots(this, MONTHDAY);
      checkPartial(temporalMonthDayLike);
      const base = rec();
      base.monthCode = monthCode(s[1]);
      base.day = s[2];
      const p = prepareFields(temporalMonthDayLike, DATE_FIELDS, "partial");
      const f = mergeFields(base, p);
      const overflow = overflowOpt(getOptionsObject(options));
      return makeMonthDay(monthDayFromFields(f, overflow), s[3]);
    }
    equals(other) {
      const s = slots(this, MONTHDAY);
      const o = toMonthDay(other);
      return compareDate([s[0], s[1], s[2]], o[0]) === 0 && s[3] === o[1];
    }
    toString(options = undefined) {
      const s = slots(this, MONTHDAY);
      return monthDayToString(s, calendarNameOpt(getOptionsObject(options)));
    }
    toLocaleString(locales = undefined, options = undefined) {
      return monthDayToString(slots(this, MONTHDAY), "auto");
    }
    toJSON() {
      return monthDayToString(slots(this, MONTHDAY), "auto");
    }
    valueOf() {
      throw new TypeError("use equals() to compare Temporal.PlainMonthDay");
    }
    toPlainDate(item) {
      const s = slots(this, MONTHDAY);
      if (!isObject(item)) throw new TypeError("an object with a year is required");
      const base = rec();
      base.monthCode = monthCode(s[1]);
      base.day = s[2];
      const p = prepareFields(item, ["year"], []);
      const f = mergeFields(base, p);
      const d = dateFromFields(f, "constrain");
      return makeDate([d.year, d.month, d.day], s[3]);
    }
  }

  // ---- Temporal.Now and the namespace ----

  const systemZone = () => "UTC";
  const nowDT = (tz) => zonedDateTime(tz === undefined ? systemZone() : toTimeZone(tz), I.nowNs());

  const Now = I.objectCreate(ObjectProto);
  const NOW_METHODS = {
    instant() {
      return makeInstant(I.nowNs());
    },
    timeZoneId() {
      return systemZone();
    },
    zonedDateTimeISO(temporalTimeZoneLike = undefined) {
      const tz = temporalTimeZoneLike === undefined ? systemZone() : toTimeZone(temporalTimeZoneLike);
      return makeZoned(I.nowNs(), tz, "iso8601");
    },
    plainDateTimeISO(temporalTimeZoneLike = undefined) {
      return makeDateTime(nowDT(temporalTimeZoneLike), "iso8601");
    },
    plainDateISO(temporalTimeZoneLike = undefined) {
      return makeDate(dtDate(nowDT(temporalTimeZoneLike)), "iso8601");
    },
    plainTimeISO(temporalTimeZoneLike = undefined) {
      return makeTime(dtTime(nowDT(temporalTimeZoneLike)));
    },
  };
  const nowNames = ["instant", "timeZoneId", "zonedDateTimeISO", "plainDateTimeISO", "plainDateISO", "plainTimeISO"];
  for (let i = 0; i < nowNames.length; i++) I.defineMethod(Now, nowNames[i], NOW_METHODS[nowNames[i]]);
  I.defineValue(Now, I.toStringTag, "Temporal.Now", false, false, true);

  const Temporal = I.objectCreate(ObjectProto);
  const classes = [[PlainDate, DATE, "PlainDate"], [PlainTime, TIME, "PlainTime"],
    [PlainDateTime, DATETIME, "PlainDateTime"], [PlainYearMonth, YEARMONTH, "PlainYearMonth"],
    [PlainMonthDay, MONTHDAY, "PlainMonthDay"], [Duration, DURATION, "Duration"], [Instant, INSTANT, "Instant"],
    [ZonedDateTime, ZONED, "ZonedDateTime"]];
  for (let i = 0; i < classes.length; i++) {
    const c = classes[i];
    protos[c[1]] = c[0].prototype;
    I.defineValue(c[0].prototype, I.toStringTag, `Temporal.${c[2]}`, false, false, true);
    I.defineValue(Temporal, c[2], c[0], true, false, true);
  }
  I.defineValue(Temporal, "Now", Now, true, false, true);
  I.defineValue(Temporal, I.toStringTag, "Temporal", false, false, true);
  I.defineValue(G, "Temporal", Temporal, true, false, true);
  void protoFrom;
});
