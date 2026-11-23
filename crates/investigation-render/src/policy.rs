//! What a plan may show, and to whom. Pure data; part of every plan's generation.

use investigation_core::header::{Classification, ClassificationLevel};

/// The audience a plan is built for. Records classified above it are rendered
/// as placeholders that reveal no authored content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Audience {
    Public,
    /// The working vault's default.
    #[default]
    Internal,
    Restricted,
}

impl Audience {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Internal => "internal",
            Self::Restricted => "restricted",
        }
    }

    const fn rank(self) -> u8 {
        match self {
            Self::Public => 0,
            Self::Internal => 1,
            Self::Restricted => 2,
        }
    }
}

/// Audience and classification rules applied while planning.
///
/// Classification-aware projection extends this type; every new rule must be
/// reflected in [`ViewPolicy::fingerprint`] so a policy change yields a new
/// generation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewPolicy {
    pub audience: Audience,
}

impl ViewPolicy {
    pub const fn for_audience(audience: Audience) -> Self {
        Self { audience }
    }

    /// Whether a record with this classification may be shown in full.
    pub fn admits(&self, classification: &Classification) -> bool {
        let level = match classification.level {
            ClassificationLevel::Public => Audience::Public,
            ClassificationLevel::Internal => Audience::Internal,
            ClassificationLevel::Restricted => Audience::Restricted,
        };
        level.rank() <= self.audience.rank()
    }

    /// Stable text identifying this policy inside a generation.
    pub fn fingerprint(&self) -> String {
        format!("audience={}", self.audience.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classified(level: ClassificationLevel) -> Classification {
        Classification {
            level,
            ..Classification::default()
        }
    }

    #[test]
    fn default_policy_admits_up_to_internal() {
        let policy = ViewPolicy::default();
        assert!(policy.admits(&classified(ClassificationLevel::Public)));
        assert!(policy.admits(&classified(ClassificationLevel::Internal)));
        assert!(!policy.admits(&classified(ClassificationLevel::Restricted)));
        assert!(
            !ViewPolicy::for_audience(Audience::Public)
                .admits(&classified(ClassificationLevel::Internal))
        );
        assert_ne!(
            ViewPolicy::default().fingerprint(),
            ViewPolicy::for_audience(Audience::Restricted).fingerprint()
        );
    }
}
