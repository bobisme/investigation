//! What a plan may show, and to whom. Pure data; part of every plan's generation.

use investigation_core::{
    classification::{self, AudiencePolicy},
    header::{Classification, ClassificationLevel},
};

use crate::compat::Resolved;

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
    /// Whether reviewed derivative artifacts the audience may see can be
    /// copied into `Attachments/` as previews ([`crate::attachments`]). Off
    /// by default; originals are never copied.
    pub derivative_previews: bool,
    /// The optional viewer features this plan may use, resolved from the
    /// compatibility profile ([`crate::compat`]). The default is the
    /// baseline: plain Markdown, no optional feature.
    pub viewer: Resolved,
}

impl ViewPolicy {
    pub const fn for_audience(audience: Audience) -> Self {
        Self {
            audience,
            derivative_previews: false,
            viewer: Resolved {
                release: None,
                optional: std::collections::BTreeSet::new(),
            },
        }
    }

    /// The same policy with derivative previews allowed or not.
    #[must_use]
    pub const fn with_derivative_previews(mut self, allowed: bool) -> Self {
        self.derivative_previews = allowed;
        self
    }

    /// The same policy with the optional viewer features `viewer` allows.
    #[must_use]
    pub fn with_viewer(mut self, viewer: Resolved) -> Self {
        self.viewer = viewer;
        self
    }

    /// Whether the plan may use one optional viewer feature, by its
    /// compatibility-profile ID such as `canvas.file`.
    pub fn allows(&self, feature: &str) -> bool {
        self.viewer.optional.contains(feature)
    }

    /// Whether the plan may use every one of `features`: how optional
    /// files that need several features (native Bases, the case map) are
    /// gated.
    pub fn allows_all(&self, features: &[&str]) -> bool {
        features.iter().all(|feature| self.allows(feature))
    }

    /// The core audience policy templates pass to
    /// [`investigation_core::classification::Classifier::may_show`]. The
    /// working vault keeps ID-only placeholders for withheld records.
    pub const fn audience_policy(&self) -> AudiencePolicy {
        let audience = match self.audience {
            Audience::Public => classification::Audience::Public,
            Audience::Internal => classification::Audience::Internal,
            Audience::Restricted => classification::Audience::Restricted,
        };
        AudiencePolicy {
            audience,
            placeholders: true,
        }
    }

    /// Whether a record with this recorded classification may be shown in
    /// full. Templates use the effective level from a `Classifier` instead,
    /// which also accounts for derivation inputs, case floors and changes.
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
        let previews = if self.derivative_previews {
            ";derivative_previews"
        } else {
            ""
        };
        let viewer = self
            .viewer
            .release
            .as_deref()
            .map_or_else(String::new, |release| format!(";viewer={release}"));
        format!("audience={}{previews}{viewer}", self.audience.as_str())
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
        assert_eq!(ViewPolicy::default().fingerprint(), "audience=internal");
        let viewer = crate::compat::Profile::bundled().resolve(Some("1.15.1"));
        let with_viewer = ViewPolicy::default().with_viewer(viewer);
        assert_eq!(with_viewer.fingerprint(), "audience=internal;viewer=1.15.1");
        assert!(with_viewer.allows("canvas.file") && !ViewPolicy::default().allows("canvas.file"));
        assert_ne!(
            ViewPolicy::default().fingerprint(),
            ViewPolicy::default()
                .with_derivative_previews(true)
                .fingerprint()
        );
        assert!(!ViewPolicy::default().allows_all(crate::bases::FEATURES));
        assert!(!ViewPolicy::default().allows_all(crate::map::FEATURES));
    }
}
