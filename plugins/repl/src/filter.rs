//! Which events and channel samples are printed.

/// `*` stands for any text, `?` for one character; the rest is compared as written.
pub fn glob(pattern: &str, text: &str) -> bool {
    let (pattern, text): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    // Where the last `*` was, and what it had taken then: the place to try again from.
    let (mut p, mut t, mut star) = (0, 0, None);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            (p, t) = (p + 1, t + 1);
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((at, taken)) = star {
            (p, t, star) = (at + 1, taken + 1, Some((at, taken + 1)));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// For the lists of the dictionary: a pattern without `*` or `?` is looked for anywhere in the
/// name, in any case.
pub fn found(pattern: &str, name: &str) -> bool {
    if pattern.contains(['*', '?']) {
        glob(pattern, name)
    } else {
        name.to_lowercase().contains(&pattern.to_lowercase())
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Filter {
    pub mute_events: bool,
    pub mute_channels: bool,
    /// Names that match one of these are not printed.
    pub muted: Vec<String>,
    /// When not empty, only names that match one of these are printed.
    pub subscribed: Vec<String>,
}

impl Filter {
    /// Whether the event (`event`) or channel sample called `name` is printed.
    pub fn allows(&self, event: bool, name: &str) -> bool {
        let whole = if event { self.mute_events } else { self.mute_channels };
        !whole
            && (self.subscribed.is_empty() || self.subscribed.iter().any(|p| glob(p, name)))
            && !self.muted.iter().any(|p| glob(p, name))
    }

    /// `mute` and `unmute`. `false` when there was nothing to change.
    pub fn mute(&mut self, on: bool, target: &str) -> bool {
        match target {
            "events" => std::mem::replace(&mut self.mute_events, on) != on,
            "channels" => std::mem::replace(&mut self.mute_channels, on) != on,
            pattern => change(&mut self.muted, on, pattern),
        }
    }

    /// `subscribe` and `unsubscribe`. `false` when there was nothing to change.
    pub fn subscribe(&mut self, on: bool, pattern: &str) -> bool {
        change(&mut self.subscribed, on, pattern)
    }

    /// What `filters` prints.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (muted, what) in [(self.mute_events, "events"), (self.mute_channels, "channels")] {
            if muted {
                lines.push(format!("muted: all {what}"));
            }
        }
        lines.extend(self.muted.iter().map(|p| format!("muted: {p}")));
        lines.extend(self.subscribed.iter().map(|p| format!("subscribed: {p}")));
        if lines.is_empty() {
            lines.push("no filter: every event and channel sample is printed".into());
        }
        lines
    }
}

fn change(patterns: &mut Vec<String>, add: bool, pattern: &str) -> bool {
    let at = patterns.iter().position(|p| p == pattern);
    match (add, at) {
        (true, None) => patterns.push(pattern.to_string()),
        (false, Some(at)) => drop(patterns.remove(at)),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob("Ref.*", "Ref.systemResources.CPU_00"));
        assert!(glob("Ref.systemResources.CPU_*", "Ref.systemResources.CPU_00"));
        assert!(glob("*.CPU_??", "Ref.systemResources.CPU_00"));
        assert!(glob("*", ""));
        assert!(glob("a*b*c", "a-b-b-c"));
        assert!(glob("Ref.a.B", "Ref.a.B"));
        assert!(!glob("Ref.*.CPU_*", "CdhCore.cmdDisp.NoOpReceived"));
        assert!(!glob("CPU_?", "CPU_00"));
        assert!(!glob("Ref.a", "Ref.a.B"));
        assert!(!glob("a*b", "a-b-c"));
    }

    #[test]
    fn a_plain_pattern_is_looked_for_anywhere_in_a_list() {
        assert!(found("no_op", "CdhCore.cmdDisp.CMD_NO_OP"));
        assert!(found("*.CMD_NO_OP", "CdhCore.cmdDisp.CMD_NO_OP"));
        assert!(!found("CMD_NO_OP?", "CdhCore.cmdDisp.CMD_NO_OP"));
    }

    #[test]
    fn subscriptions_then_mutes() {
        let mut filter = Filter::default();
        assert!(filter.allows(true, "A.b.C") && filter.allows(false, "A.b.C"));
        assert!(filter.subscribe(true, "Ref.*"));
        assert!(!filter.subscribe(true, "Ref.*"));
        assert!(filter.allows(false, "Ref.systemResources.CPU_00"));
        assert!(!filter.allows(true, "CdhCore.cmdDisp.NoOpReceived"));
        assert!(filter.mute(true, "Ref.systemResources.CPU_*"));
        assert!(!filter.allows(false, "Ref.systemResources.CPU_00"));
        assert!(filter.allows(false, "Ref.systemResources.MEMORY_TOTAL"));
        assert!(filter.mute(false, "Ref.systemResources.CPU_*") && !filter.mute(false, "Ref.systemResources.CPU_*"));
        assert!(filter.subscribe(false, "Ref.*"));
        assert_eq!(filter, Filter::default());
    }

    #[test]
    fn a_whole_stream_is_muted() {
        let mut filter = Filter::default();
        assert!(filter.mute(true, "channels"));
        assert!(filter.allows(true, "A.b.C") && !filter.allows(false, "A.b.C"));
        assert!(filter.mute(true, "events") && !filter.mute(true, "events"));
        assert!(!filter.allows(true, "A.b.C"));
        assert_eq!(filter.lines(), ["muted: all events", "muted: all channels"]);
        assert!(filter.mute(false, "events") && filter.mute(false, "channels"));
        assert_eq!(filter.lines().len(), 1);
    }
}
