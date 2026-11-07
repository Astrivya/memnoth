//! What will hold a command, on the platform that is running.
//!
//! A [`BackendPlan`](crate::backend::BackendPlan) is data on every OS. This is
//! the one type that says what happened to it: either a program and argv that
//! apply the plan (`bwrap` on Linux, `sandbox-exec` on macOS), or the reason
//! there is no boundary. The caller prints `describe()` next to the command, so
//! "confined" and "not confined" can never sound alike.

/// How one command will run, once a plan has been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confinement {
    /// Run this program with these arguments: the plan is applied by `bwrap`.
    Bwrap { program: String, args: Vec<String> },
    /// Run this program with these arguments: the plan is applied by macOS
    /// `sandbox-exec`, with the profile carried as `-p <text>`.
    Seatbelt { program: String, args: Vec<String> },
    /// Nothing will confine the command, and this is why. The caller prints the
    /// reason next to the command rather than letting the person assume.
    Unavailable { reason: String },
}

impl Confinement {
    /// Whether a process boundary exists for this command.
    pub fn is_enforced(&self) -> bool {
        matches!(self, Self::Bwrap { .. } | Self::Seatbelt { .. })
    }

    /// The program and arguments that actually run, when a boundary applies.
    pub fn confined_argv(&self) -> Option<(String, Vec<String>)> {
        match self {
            Self::Bwrap { program, args } | Self::Seatbelt { program, args } => {
                Some((program.clone(), args.clone()))
            }
            Self::Unavailable { .. } => None,
        }
    }

    /// The sentence that belongs on screen, whoever prints it.
    pub fn describe(&self) -> String {
        match self {
            Self::Bwrap { .. } => {
                "confined: bwrap (read-only filesystem, workspace write, deny paths masked, \
                 network per profile)"
                    .to_string()
            }
            Self::Seatbelt { .. } => {
                "confined: sandbox-exec/Seatbelt (read-only filesystem, workspace write, deny \
                 paths hidden, network per profile)"
                    .to_string()
            }
            Self::Unavailable { reason } => format!("not confined: {reason}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_real_backend_counts_as_confined() {
        assert!(
            Confinement::Bwrap {
                program: "bwrap".into(),
                args: Vec::new()
            }
            .is_enforced()
        );
        assert!(
            Confinement::Seatbelt {
                program: "sandbox-exec".into(),
                args: Vec::new()
            }
            .is_enforced()
        );
        assert!(
            !Confinement::Unavailable {
                reason: "no backend".into()
            }
            .is_enforced()
        );
    }

    #[test]
    fn an_unavailable_confinement_never_pretends_to_have_an_argv() {
        let unavailable = Confinement::Unavailable {
            reason: "no backend".into(),
        };
        assert!(unavailable.confined_argv().is_none());
        assert!(unavailable.describe().starts_with("not confined:"));
    }
}
