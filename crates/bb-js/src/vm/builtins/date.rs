//! Date (§21.4). Local time is UTC for now (LocalTZA = 0, an implementation choice the spec
//! allows); time zone support comes with the browser integration.

use super::super::heap::*;
use super::super::ops::Hint;
use super::super::realm::{self, method, method_sym};
use super::super::value::{PropKey, Value};
use super::super::{JsResult, Vm};

const MS_PER_DAY: f64 = 86_400_000.0;

fn day(t: f64) -> f64 {
    (t / MS_PER_DAY).floor()
}
fn time_within_day(t: f64) -> f64 {
    t.rem_euclid(MS_PER_DAY)
}
fn days_in_year(y: f64) -> f64 {
    if y % 4.0 != 0.0 {
        365.0
    } else if y % 100.0 != 0.0 {
        366.0
    } else if y % 400.0 != 0.0 {
        365.0
    } else {
        366.0
    }
}
fn day_from_year(y: f64) -> f64 {
    365.0 * (y - 1970.0) + ((y - 1969.0) / 4.0).floor() - ((y - 1901.0) / 100.0).floor()
        + ((y - 1601.0) / 400.0).floor()
}
fn time_from_year(y: f64) -> f64 {
    MS_PER_DAY * day_from_year(y)
}
fn year_from_time(t: f64) -> f64 {
    let mut y = (t / (MS_PER_DAY * 365.2425)).floor() + 1970.0;
    while time_from_year(y) > t {
        y -= 1.0;
    }
    while time_from_year(y + 1.0) <= t {
        y += 1.0;
    }
    y
}
fn in_leap_year(t: f64) -> bool {
    days_in_year(year_from_time(t)) == 366.0
}
fn day_within_year(t: f64) -> f64 {
    day(t) - day_from_year(year_from_time(t))
}
const MONTH_STARTS: [f64; 13] =
    [0.0, 31.0, 59.0, 90.0, 120.0, 151.0, 181.0, 212.0, 243.0, 273.0, 304.0, 334.0, 365.0];
fn month_start(m: usize, leap: bool) -> f64 {
    MONTH_STARTS[m] + if leap && m >= 2 { 1.0 } else { 0.0 }
}
fn month_from_time(t: f64) -> f64 {
    let d = day_within_year(t);
    let leap = in_leap_year(t);
    (0..12).find(|&m| d < month_start(m + 1, leap)).unwrap_or(11) as f64
}
fn date_from_time(t: f64) -> f64 {
    let d = day_within_year(t);
    let m = month_from_time(t) as usize;
    d - month_start(m, in_leap_year(t)) + 1.0
}
fn week_day(t: f64) -> f64 {
    (day(t) + 4.0).rem_euclid(7.0)
}
fn hour_from_time(t: f64) -> f64 {
    (t / 3_600_000.0).floor().rem_euclid(24.0)
}
fn min_from_time(t: f64) -> f64 {
    (t / 60_000.0).floor().rem_euclid(60.0)
}
fn sec_from_time(t: f64) -> f64 {
    (t / 1000.0).floor().rem_euclid(60.0)
}
fn ms_from_time(t: f64) -> f64 {
    t.rem_euclid(1000.0)
}

fn to_int(x: f64) -> f64 {
    x.trunc() + 0.0
}

/// MakeTime (§21.4.1.27).
fn make_time(h: f64, m: f64, s: f64, ms: f64) -> f64 {
    if !h.is_finite() || !m.is_finite() || !s.is_finite() || !ms.is_finite() {
        return f64::NAN;
    }
    to_int(h) * 3_600_000.0 + to_int(m) * 60_000.0 + to_int(s) * 1000.0 + to_int(ms)
}

/// MakeDay (§21.4.1.28).
fn make_day(y: f64, m: f64, d: f64) -> f64 {
    if !y.is_finite() || !m.is_finite() || !d.is_finite() {
        return f64::NAN;
    }
    let (y, m, d) = (to_int(y), to_int(m), to_int(d));
    let ym = y + (m / 12.0).floor();
    if ym.abs() > 400_000.0 {
        return f64::NAN;
    }
    let mn = m.rem_euclid(12.0) as usize;
    let t = time_from_year(ym);
    let leap = days_in_year(ym) == 366.0;
    day(t) + month_start(mn, leap) + d - 1.0
}

fn make_date(day: f64, time: f64) -> f64 {
    if !day.is_finite() || !time.is_finite() {
        return f64::NAN;
    }
    let tv = day * MS_PER_DAY + time;
    if !tv.is_finite() {
        return f64::NAN;
    }
    tv
}

/// TimeClip (§21.4.1.31).
fn time_clip(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > 8.64e15 {
        return f64::NAN;
    }
    to_int(t)
}

// Local time = UTC (see the module comment).
fn local(t: f64) -> f64 {
    t
}
fn utc(t: f64) -> f64 {
    t
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn year_str(y: f64) -> String {
    if y >= 0.0 {
        format!("{:04}", y as i64)
    } else {
        format!("-{:04}", (-y) as i64)
    }
}

/// DateString (§21.4.4.41.2).
fn date_string(t: f64) -> String {
    format!(
        "{} {} {:02} {}",
        DAYS[week_day(t) as usize],
        MONTHS[month_from_time(t) as usize],
        date_from_time(t) as i64,
        year_str(year_from_time(t))
    )
}

/// TimeString + TimeZoneString (§21.4.4.41.1, .3).
fn time_string(t: f64) -> String {
    format!(
        "{:02}:{:02}:{:02} GMT+0000 (Coordinated Universal Time)",
        hour_from_time(t) as i64,
        min_from_time(t) as i64,
        sec_from_time(t) as i64
    )
}

fn iso_string(t: f64) -> String {
    let y = year_from_time(t);
    let ys = if (0.0..=9999.0).contains(&y) {
        format!("{:04}", y as i64)
    } else if y < 0.0 {
        format!("-{:06}", (-y) as i64)
    } else {
        format!("+{:06}", y as i64)
    };
    format!(
        "{ys}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        month_from_time(t) as i64 + 1,
        date_from_time(t) as i64,
        hour_from_time(t) as i64,
        min_from_time(t) as i64,
        sec_from_time(t) as i64,
        ms_from_time(t) as i64
    )
}

/// Date.parse: the Date Time String Format (§21.4.1.32) and the toString/toUTCString forms.
fn parse_date(s: &str) -> f64 {
    parse_iso(s).or_else(|| parse_to_string_form(s)).unwrap_or(f64::NAN)
}

fn parse_iso(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = 0;
    let num = |i: &mut usize, n: usize| -> Option<f64> {
        let st = b.get(*i..*i + n)?;
        if !st.iter().all(u8::is_ascii_digit) {
            return None;
        }
        *i += n;
        std::str::from_utf8(st).ok()?.parse().ok()
    };
    let year = match b.first() {
        Some(b'+') | Some(b'-') => {
            let neg = b[0] == b'-';
            i = 1;
            let y = num(&mut i, 6)?;
            if neg && y == 0.0 {
                return None;
            }
            if neg {
                -y
            } else {
                y
            }
        }
        _ => num(&mut i, 4)?,
    };
    let (mut month, mut dayv) = (1.0, 1.0);
    if b.get(i) == Some(&b'-') {
        i += 1;
        month = num(&mut i, 2)?;
        if b.get(i) == Some(&b'-') {
            i += 1;
            dayv = num(&mut i, 2)?;
        }
    }
    let (mut h, mut mi, mut sec, mut ms) = (0.0, 0.0, 0.0, 0.0);
    let mut offset: Option<f64> = None;
    let mut has_time = false;
    if b.get(i) == Some(&b'T') {
        has_time = true;
        i += 1;
        h = num(&mut i, 2)?;
        if b.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        mi = num(&mut i, 2)?;
        if b.get(i) == Some(&b':') {
            i += 1;
            sec = num(&mut i, 2)?;
            if b.get(i) == Some(&b'.') {
                i += 1;
                let st = i;
                while b.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                if i == st {
                    return None;
                }
                let frac = &s[st..i.min(st + 3)];
                ms = format!("{frac:0<3}").parse().ok()?;
            }
        }
        match b.get(i) {
            Some(b'Z') => {
                offset = Some(0.0);
                i += 1;
            }
            Some(&c) if c == b'+' || c == b'-' => {
                i += 1;
                let oh = num(&mut i, 2)?;
                if b.get(i) != Some(&b':') {
                    return None;
                }
                i += 1;
                let om = num(&mut i, 2)?;
                if oh > 23.0 || om > 59.0 {
                    return None;
                }
                let o = (oh * 60.0 + om) * 60_000.0;
                offset = Some(if c == b'+' { o } else { -o });
            }
            _ => {}
        }
    }
    if i != b.len() {
        return None;
    }
    if !(1.0..=12.0).contains(&month) || dayv < 1.0 || h > 24.0 || mi > 59.0 || sec > 59.0 {
        return None;
    }
    if h == 24.0 && (mi != 0.0 || sec != 0.0 || ms != 0.0) {
        return None;
    }
    let leap = days_in_year(year) == 366.0;
    let m = month as usize - 1;
    if dayv > month_start(m + 1, leap) - month_start(m, leap) {
        return None;
    }
    let t = make_date(make_day(year, month - 1.0, dayv), make_time(h, mi, sec, ms));
    // Date-only forms are UTC; date-time forms without an offset are local time.
    let t = match offset {
        Some(o) => t - o,
        None if has_time => utc(t),
        None => t,
    };
    let c = time_clip(t);
    if c.is_nan() {
        return None;
    }
    Some(c)
}

/// "Tue Feb 01 2022 00:00:00 GMT+0000 (…)" and "Tue, 01 Feb 2022 00:00:00 GMT".
fn parse_to_string_form(s: &str) -> Option<f64> {
    let s = s.replace(',', "");
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let mon = |p: &str| MONTHS.iter().position(|m| *m == p);
    let (m, d, y, rest) = if let Some(m) = mon(parts[1]) {
        (m, parts[2], parts[3], &parts[4..])
    } else {
        let m = mon(parts[2])?;
        (m, parts[1], parts[3], &parts[4..])
    };
    let d: f64 = d.parse().ok()?;
    let y: f64 = y.parse().ok()?;
    let (mut h, mut mi, mut sec, mut off) = (0.0, 0.0, 0.0, 0.0);
    if let Some(t) = rest.first() {
        let hms: Vec<&str> = t.split(':').collect();
        if hms.len() != 3 {
            return None;
        }
        h = hms[0].parse().ok()?;
        mi = hms[1].parse().ok()?;
        sec = hms[2].parse().ok()?;
        if let Some(z) = rest.get(1) {
            if let Some(o) = z.strip_prefix("GMT") {
                if !o.is_empty() {
                    let sign = if o.starts_with('-') { -1.0 } else { 1.0 };
                    let digits: String = o.chars().filter(|c| c.is_ascii_digit()).collect();
                    if digits.len() != 4 {
                        return None;
                    }
                    let v: f64 = digits.parse().ok()?;
                    off = sign * ((v / 100.0).floor() * 60.0 + v % 100.0) * 60_000.0;
                }
            }
        }
    }
    let t = make_date(make_day(y, m as f64, d), make_time(h, mi, sec, 0.0)) - off;
    let c = time_clip(t);
    (!c.is_nan()).then_some(c)
}

pub(super) fn this_time(vm: &mut Vm, v: &Value) -> JsResult<f64> {
    match v {
        Value::Object(o) => match vm.heap.obj(*o).kind {
            Kind::Date(t) => Ok(t),
            _ => vm.type_error("this is not a Date object."),
        },
        _ => vm.type_error("this is not a Date object."),
    }
}

fn set_time(vm: &mut Vm, v: &Value, t: f64) -> Value {
    if let Value::Object(o) = v {
        if let Kind::Date(x) = &mut vm.heap.obj_mut(*o).kind {
            *x = t;
        }
    }
    Value::Number(t)
}

fn args_num(vm: &mut Vm, c: &NativeCall) -> JsResult<Vec<f64>> {
    let mut out = Vec::with_capacity(c.args.len());
    for a in &c.args {
        out.push(vm.to_number(a)?);
    }
    Ok(out)
}

/// The fields a setter changes: which component the first argument sets.
#[derive(Clone, Copy)]
enum Field {
    Ms,
    Sec,
    Min,
    Hour,
    Date,
    Month,
    Year,
}

/// set{UTC}{Milliseconds,Seconds,Minutes,Hours,Date,Month,FullYear} (§21.4.4.20–29).
fn setter(vm: &mut Vm, c: NativeCall, field: Field, is_utc: bool) -> JsResult<Value> {
    let t0 = this_time(vm, &c.this)?;
    let args = args_num(vm, &c)?;
    let arg = |i: usize| args.get(i).copied();
    let base = if t0.is_nan() {
        if !matches!(field, Field::Year) {
            return Ok(Value::Number(f64::NAN));
        }
        0.0
    } else if is_utc {
        t0
    } else {
        local(t0)
    };
    let (mut y, mut mo, mut d) = (year_from_time(base), month_from_time(base), date_from_time(base));
    let (mut h, mut mi, mut s, mut ms) =
        (hour_from_time(base), min_from_time(base), sec_from_time(base), ms_from_time(base));
    let a0 = arg(0).unwrap_or(f64::NAN);
    match field {
        Field::Ms => ms = a0,
        Field::Sec => {
            s = a0;
            ms = arg(1).unwrap_or(ms);
        }
        Field::Min => {
            mi = a0;
            s = arg(1).unwrap_or(s);
            ms = arg(2).unwrap_or(ms);
        }
        Field::Hour => {
            h = a0;
            mi = arg(1).unwrap_or(mi);
            s = arg(2).unwrap_or(s);
            ms = arg(3).unwrap_or(ms);
        }
        Field::Date => d = a0,
        Field::Month => {
            mo = a0;
            d = arg(1).unwrap_or(d);
        }
        Field::Year => {
            y = a0;
            mo = arg(1).unwrap_or(mo);
            d = arg(2).unwrap_or(d);
        }
    }
    let nd = make_date(make_day(y, mo, d), make_time(h, mi, s, ms));
    let v = time_clip(if is_utc { nd } else { utc(nd) });
    Ok(set_time(vm, &c.this, v))
}

fn date_value_args(vm: &mut Vm, c: &NativeCall) -> JsResult<f64> {
    let n = args_num(vm, c)?;
    let y = n[0];
    let m = n.get(1).copied().unwrap_or(0.0);
    let d = n.get(2).copied().unwrap_or(1.0);
    let h = n.get(3).copied().unwrap_or(0.0);
    let mi = n.get(4).copied().unwrap_or(0.0);
    let s = n.get(5).copied().unwrap_or(0.0);
    let ms = n.get(6).copied().unwrap_or(0.0);
    let yr = if y.is_nan() {
        f64::NAN
    } else {
        let yi = to_int(y);
        if (0.0..=99.0).contains(&yi) {
            1900.0 + yi
        } else {
            y
        }
    };
    Ok(make_date(make_day(yr, m, d), make_time(h, mi, s, ms)))
}

fn ctor(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let Some(nt) = c.new_target else {
        let t = now();
        return Ok(Value::str(&format!("{} {}", date_string(local(t)), time_string(local(t)))));
    };
    let tv = match c.args.len() {
        0 => now(),
        1 => {
            let v = c.arg(0);
            let tv = match &v {
                Value::Object(o) if matches!(vm.heap.obj(*o).kind, Kind::Date(_)) => this_time(vm, &v)?,
                _ => {
                    let p = vm.to_primitive(&v, Hint::Default)?;
                    if let Value::String(s) = &p {
                        parse_date(&String::from_utf16_lossy(s))
                    } else {
                        vm.to_number(&p)?
                    }
                }
            };
            time_clip(tv)
        }
        _ => time_clip(utc(date_value_args(vm, &c)?)),
    };
    let proto = vm.prototype_from_ctor(nt, |r| r.date_proto)?;
    Ok(Value::Object(vm.heap.alloc(Object::new(Some(proto), Kind::Date(tv)))))
}

pub(super) fn install(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let dp = vm.new_object_with_proto(Some(op));
    vm.realm.date_proto = dp;
    let dc = realm::constructor(vm, "Date", 7, ctor, dp);
    method(vm, dc, "now", 0, |_, _| Ok(Value::Number(now())));
    method(vm, dc, "parse", 1, |vm, c| {
        let s = super::arg_string(vm, &c, 0)?;
        Ok(Value::Number(parse_date(&String::from_utf16_lossy(&s))))
    });
    method(vm, dc, "UTC", 7, |vm, c| {
        if c.args.is_empty() {
            return Ok(Value::Number(f64::NAN));
        }
        Ok(Value::Number(time_clip(date_value_args(vm, &c)?)))
    });

    macro_rules! getters {
        ($($name:literal => $f:expr, $local:expr);* $(;)?) => {
            $(method(vm, dp, $name, 0, |vm, c| {
                let t = this_time(vm, &c.this)?;
                if t.is_nan() {
                    return Ok(Value::Number(f64::NAN));
                }
                let t = if $local { local(t) } else { t };
                let f: fn(f64) -> f64 = $f;
                Ok(Value::Number(f(t)))
            });)*
        };
    }
    getters!(
        "getDate" => date_from_time, true;
        "getDay" => week_day, true;
        "getFullYear" => year_from_time, true;
        "getHours" => hour_from_time, true;
        "getMilliseconds" => ms_from_time, true;
        "getMinutes" => min_from_time, true;
        "getMonth" => month_from_time, true;
        "getSeconds" => sec_from_time, true;
        "getYear" => |t| year_from_time(t) - 1900.0, true;
        "getUTCDate" => date_from_time, false;
        "getUTCDay" => week_day, false;
        "getUTCFullYear" => year_from_time, false;
        "getUTCHours" => hour_from_time, false;
        "getUTCMilliseconds" => ms_from_time, false;
        "getUTCMinutes" => min_from_time, false;
        "getUTCMonth" => month_from_time, false;
        "getUTCSeconds" => sec_from_time, false;
        "getTimezoneOffset" => |_| 0.0, false;
    );
    method(vm, dp, "getTime", 0, |vm, c| Ok(Value::Number(this_time(vm, &c.this)?)));
    method(vm, dp, "valueOf", 0, |vm, c| Ok(Value::Number(this_time(vm, &c.this)?)));
    method(vm, dp, "setTime", 1, |vm, c| {
        this_time(vm, &c.this)?;
        let t = vm.to_number(&c.arg(0))?;
        Ok(set_time(vm, &c.this, time_clip(t)))
    });
    macro_rules! setters {
        ($($name:literal, $len:expr => $field:expr, $utc:expr);* $(;)?) => {
            $(method(vm, dp, $name, $len, |vm, c| setter(vm, c, $field, $utc));)*
        };
    }
    setters!(
        "setMilliseconds", 1 => Field::Ms, false;
        "setSeconds", 2 => Field::Sec, false;
        "setMinutes", 3 => Field::Min, false;
        "setHours", 4 => Field::Hour, false;
        "setDate", 1 => Field::Date, false;
        "setMonth", 2 => Field::Month, false;
        "setFullYear", 3 => Field::Year, false;
        "setUTCMilliseconds", 1 => Field::Ms, true;
        "setUTCSeconds", 2 => Field::Sec, true;
        "setUTCMinutes", 3 => Field::Min, true;
        "setUTCHours", 4 => Field::Hour, true;
        "setUTCDate", 1 => Field::Date, true;
        "setUTCMonth", 2 => Field::Month, true;
        "setUTCFullYear", 3 => Field::Year, true;
    );
    // Annex B.2.3.2 setYear.
    method(vm, dp, "setYear", 1, |vm, c| {
        let t = this_time(vm, &c.this)?;
        let y = vm.to_number(&c.arg(0))?;
        if y.is_nan() {
            return Ok(set_time(vm, &c.this, f64::NAN));
        }
        let yi = to_int(y);
        let yyyy = if (0.0..=99.0).contains(&yi) { yi + 1900.0 } else { y };
        let base = if t.is_nan() { 0.0 } else { local(t) };
        let d = make_day(yyyy, month_from_time(base), date_from_time(base));
        let v = time_clip(utc(make_date(d, time_within_day(base))));
        Ok(set_time(vm, &c.this, v))
    });
    macro_rules! strings {
        ($($name:literal => $f:expr);* $(;)?) => {
            $(method(vm, dp, $name, 0, |vm, c| {
                let t = this_time(vm, &c.this)?;
                if t.is_nan() {
                    return Ok(Value::str("Invalid Date"));
                }
                let f: fn(f64) -> String = $f;
                Ok(Value::str(&f(t)))
            });)*
        };
    }
    strings!(
        "toString" => |t| format!("{} {}", date_string(local(t)), time_string(local(t)));
        "toDateString" => |t| date_string(local(t));
        "toTimeString" => |t| time_string(local(t));
        "toLocaleString" => |t| format!("{} {}", date_string(local(t)), time_string(local(t)));
        "toLocaleDateString" => |t| date_string(local(t));
        "toLocaleTimeString" => |t| time_string(local(t));
    );
    let utc_fn = method(vm, dp, "toUTCString", 0, |vm, c| {
        let t = this_time(vm, &c.this)?;
        if t.is_nan() {
            return Ok(Value::str("Invalid Date"));
        }
        Ok(Value::str(&format!(
            "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
            DAYS[week_day(t) as usize],
            date_from_time(t) as i64,
            MONTHS[month_from_time(t) as usize],
            year_str(year_from_time(t)),
            hour_from_time(t) as i64,
            min_from_time(t) as i64,
            sec_from_time(t) as i64
        )))
    });
    // Annex B.2.3.3: toGMTString is the same function object.
    realm::value(vm, dp, "toGMTString", Value::Object(utc_fn));
    method(vm, dp, "toISOString", 0, |vm, c| {
        let t = this_time(vm, &c.this)?;
        if t.is_nan() {
            return vm.range_error("Invalid time value");
        }
        Ok(Value::str(&iso_string(t)))
    });
    method(vm, dp, "toJSON", 1, |vm, c| {
        let o = vm.to_object(&c.this)?;
        let tv = vm.to_primitive(&Value::Object(o), Hint::Number)?;
        if let Value::Number(n) = tv {
            if !n.is_finite() {
                return Ok(Value::Null);
            }
        }
        let f = vm.get(o, &PropKey::lit("toISOString"))?;
        vm.call(&f, Value::Object(o), Vec::new())
    });
    let tp = vm.realm.sym_to_primitive.clone();
    method_sym(vm, dp, tp.clone(), "[Symbol.toPrimitive]", 1, |vm, c| {
        let Value::Object(o) = c.this else {
            return vm.type_error("Date.prototype[Symbol.toPrimitive] called on non-object");
        };
        let hint = match c.arg(0) {
            Value::String(s)
                if s.iter().copied().eq("string".encode_utf16())
                    || s.iter().copied().eq("default".encode_utf16()) =>
            {
                Hint::String
            }
            Value::String(s) if s.iter().copied().eq("number".encode_utf16()) => Hint::Number,
            _ => return vm.type_error("Invalid hint"),
        };
        vm.ordinary_to_primitive(o, hint)
    });
    if let Some(p) = vm.heap.obj_mut(dp).props.get_mut(&PropKey::Sym(tp)) {
        p.writable = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_math() {
        let t = make_date(make_day(2024.0, 1.0, 29.0), make_time(12.0, 30.0, 0.0, 5.0));
        assert_eq!((year_from_time(t), month_from_time(t), date_from_time(t)), (2024.0, 1.0, 29.0));
        assert_eq!(week_day(t), 4.0);
        assert_eq!(iso_string(t), "2024-02-29T12:30:00.005Z");
        assert_eq!(parse_date("2024-02-29T12:30:00.005Z"), t);
        assert_eq!(parse_date("1970-01-01"), 0.0);
        assert!(parse_date("2023-02-29").is_nan());
        assert_eq!(parse_date("Thu, 01 Jan 1970 00:00:00 GMT"), 0.0);
        assert_eq!(parse_date("Thu Jan 01 1970 00:00:00 GMT+0100 (CET)"), -3_600_000.0);
        assert_eq!(year_from_time(-1.0), 1969.0);
        assert_eq!(iso_string(-62198755200000.0), "-000001-01-01T00:00:00.000Z");
    }
}
