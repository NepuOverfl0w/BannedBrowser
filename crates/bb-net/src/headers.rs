//! A case-insensitive, order-preserving header list.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    list: Vec<(String, String)>,
}

impl Headers {
    pub fn new() -> Self {
        Self::default()
    }

    /// First value for `name` (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.list.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// All values for `name`.
    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.list.iter().filter(move |(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// Values for `name` combined with `, ` (Fetch "get" semantics).
    pub fn get_combined(&self, name: &str) -> Option<String> {
        let v: Vec<&str> = self.get_all(name).collect();
        if v.is_empty() {
            None
        } else {
            Some(v.join(", "))
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Replace all values for `name` with `value`.
    pub fn set(&mut self, name: &str, value: &str) {
        self.remove(name);
        self.list.push((name.to_string(), value.to_string()));
    }

    pub fn append(&mut self, name: &str, value: &str) {
        self.list.push((name.to_string(), value.to_string()));
    }

    pub fn remove(&mut self, name: &str) {
        self.list.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.list.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Whether a comma-separated header contains `token` (case-insensitive).
    pub fn has_token(&self, name: &str, token: &str) -> bool {
        self.get_all(name).flat_map(|v| v.split(',')).any(|t| t.trim().eq_ignore_ascii_case(token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn case_insensitive() {
        let mut h = Headers::new();
        h.append("Content-Type", "text/html");
        h.append("Set-Cookie", "a=1");
        h.append("set-cookie", "b=2");
        assert_eq!(h.get("content-type"), Some("text/html"));
        assert_eq!(h.get_all("SET-COOKIE").count(), 2);
        h.set("CONTENT-TYPE", "text/plain");
        assert_eq!(h.get("Content-Type"), Some("text/plain"));
        assert_eq!(h.len(), 3);
    }
}
