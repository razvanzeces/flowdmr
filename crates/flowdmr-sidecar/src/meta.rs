//! Parse DMR call metadata (source/radio id, talkgroup, end-of-call) out of the
//! decoder's console output.
//!
//! dsd-neo / dsd-fme print call information as free text whose exact shape varies
//! by build, so the patterns are configurable regexes (see [`crate::config`]).
//! The defaults match common output; tune them in the config file if your build
//! differs. Each capture's group 1 is the integer of interest.

use regex::Regex;

/// What we extracted from one decoder console line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetaLine {
    pub source: Option<u32>,
    pub talkgroup: Option<u32>,
    pub call_end: bool,
}

impl MetaLine {
    pub fn is_empty(&self) -> bool {
        self.source.is_none() && self.talkgroup.is_none() && !self.call_end
    }
}

/// Compiled metadata matchers.
pub struct MetaParser {
    re_source: Regex,
    re_talkgroup: Regex,
    re_call_end: Regex,
}

impl MetaParser {
    pub fn new(re_source: &str, re_talkgroup: &str, re_call_end: &str) -> Result<Self, regex::Error> {
        Ok(Self {
            re_source: Regex::new(re_source)?,
            re_talkgroup: Regex::new(re_talkgroup)?,
            re_call_end: Regex::new(re_call_end)?,
        })
    }

    pub fn parse_line(&self, line: &str) -> MetaLine {
        let cap_u32 = |re: &Regex| -> Option<u32> {
            re.captures(line)
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse::<u32>().ok())
        };
        MetaLine {
            source: cap_u32(&self.re_source),
            talkgroup: cap_u32(&self.re_talkgroup),
            call_end: self.re_call_end.is_match(line),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn parser() -> MetaParser {
        let c = Config::default();
        MetaParser::new(&c.re_source, &c.re_talkgroup, &c.re_call_end).unwrap()
    }

    #[test]
    fn parses_source_and_talkgroup_same_line() {
        let p = parser();
        let m = p.parse_line("Sync: +DMR  Source: 2604123  Target: 9  (slot 1)");
        assert_eq!(m.source, Some(2_604_123));
        assert_eq!(m.talkgroup, Some(9));
        assert!(!m.call_end);
    }

    #[test]
    fn parses_abbreviated_forms() {
        let p = parser();
        let m = p.parse_line("VC SRC=311500 TGT=31337 CC=1");
        assert_eq!(m.source, Some(311_500));
        assert_eq!(m.talkgroup, Some(31337));
    }

    #[test]
    fn detects_call_end() {
        let p = parser();
        assert!(p.parse_line("Sync: no sync").call_end);
        assert!(p.parse_line("[DMR] Voice End / Terminator with LC").call_end);
        assert!(!p.parse_line("Source: 1 Target: 2").call_end);
    }

    #[test]
    fn ignores_unrelated_lines() {
        let p = parser();
        let m = p.parse_line("Audio output device opened at 8000 Hz");
        // "8000" must not be mistaken for a source/tg (no source/target keyword).
        assert!(m.is_empty(), "got {m:?}");
    }
}
