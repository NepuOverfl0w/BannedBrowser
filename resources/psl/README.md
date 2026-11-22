# Public Suffix List

`public_suffix_list.dat` is the Public Suffix List from <https://publicsuffix.org/list/>,
shipped **unmodified**. It is licensed under the Mozilla Public License 2.0
(<https://mozilla.org/MPL/2.0/>), as stated at the top of the file.

It is data, not code: BannedBrowser's own matcher (`crates/bb-net/src/psl.rs`) reads it to find
public suffixes and registrable domains, which cookies (and later same-site checks) need.

Update it with `scripts/update-psl.sh`, then run `cargo test -p bb-net`.
