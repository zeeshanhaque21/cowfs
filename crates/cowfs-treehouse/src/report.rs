use cowfs_ctl::escape_control;
use serde::Serialize;

use crate::error::EXIT_OK;

/// One named check with a verdict, so `doctor` output is testable and greppable.
#[derive(Clone, Debug, Serialize)]
pub struct Check {
    /// What was checked.
    pub name: String,
    /// Whether it passed. A check that could not run at all is `false` with the reason in
    /// `detail`, never a silent skip.
    pub ok: bool,
    /// What was found, always printed, pass or fail.
    pub detail: String,
}

/// The result of a list of checks.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    /// Every check that ran, in order.
    pub checks: Vec<Check>,
}

impl Report {
    /// A report with no checks.
    pub fn new() -> Self {
        Report::default()
    }

    /// Records a passing check.
    pub fn pass(&mut self, name: &str, detail: impl Into<String>) {
        self.checks.push(Check {
            name: name.to_owned(),
            ok: true,
            detail: detail.into(),
        });
    }

    /// Records a failing check.
    pub fn fail(&mut self, name: &str, detail: impl Into<String>) {
        self.checks.push(Check {
            name: name.to_owned(),
            ok: false,
            detail: detail.into(),
        });
    }

    /// Records a check whose outcome is a condition.
    pub fn check(&mut self, name: &str, ok: bool, detail: impl Into<String>) -> bool {
        let detail = detail.into();
        if ok {
            self.pass(name, detail);
        } else {
            self.fail(name, detail);
        }
        ok
    }

    /// How many checks failed.
    pub fn failures(&self) -> usize {
        self.checks.iter().filter(|c| !c.ok).count()
    }

    /// Whether every check passed.
    pub fn all_ok(&self) -> bool {
        self.failures() == 0
    }

    /// The exit code a report implies: 0 when everything passed, 1 otherwise.
    pub fn exit_code(&self) -> i32 {
        if self.all_ok() {
            EXIT_OK
        } else {
            crate::error::EXIT_ERROR
        }
    }

    /// Prints the report, as one JSON line when `json`, otherwise as aligned text.
    pub fn print(&self, json: bool) {
        if json {
            let line = serde_json::to_string(self).unwrap_or_else(|e| {
                format!("{{\"checks\":[],\"error\":\"cannot render report: {e}\"}}")
            });
            println!("{line}");
            return;
        }
        for c in &self.checks {
            println!(
                "{:<5} {:<28} {}",
                if c.ok { "ok" } else { "FAIL" },
                escape_control(&c.name),
                escape_control(&c.detail)
            );
        }
        let total = self.checks.len();
        let failed = self.failures();
        println!(
            "\n{total} checks, {} failed",
            if failed == 0 {
                "none".to_owned()
            } else {
                failed.to_string()
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_of_passes_is_ok() {
        let mut r = Report::new();
        r.pass("a", "fine");
        r.check("b", true, "fine");
        assert!(r.all_ok());
        assert_eq!(r.exit_code(), EXIT_OK);
        assert_eq!(r.failures(), 0);
    }

    #[test]
    fn one_failure_fails_the_report() {
        let mut r = Report::new();
        r.pass("a", "fine");
        r.fail("b", "broken");
        assert!(!r.all_ok());
        assert_eq!(r.exit_code(), 1);
        assert_eq!(r.failures(), 1);
    }

    #[test]
    fn an_empty_report_is_ok() {
        assert!(Report::new().all_ok());
    }

    #[test]
    fn check_records_both_verdicts() {
        let mut r = Report::new();
        assert!(r.check("yes", true, "d"));
        assert!(!r.check("no", false, "d"));
        assert_eq!(r.checks.len(), 2);
    }
}
