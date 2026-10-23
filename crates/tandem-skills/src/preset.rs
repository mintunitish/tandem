use crate::SkillId;

/// One-click skill selections shown as toggles in the cockpit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    /// ponytail: laziest solution that works.
    Lazy,
    /// caveman: terse output.
    Terse,
    /// superpowers: brainstorming, TDD, systematic debugging, verification.
    Disciplined,
    /// gstack: review, ship, QA.
    Ship,
    /// Tandem's own skills (review, db-safety, worktree-etiquette, harness).
    TandemDefault,
}

impl Preset {
    pub const ALL: [Preset; 5] =
        [Preset::Lazy, Preset::Terse, Preset::Disciplined, Preset::Ship, Preset::TandemDefault];

    pub fn name(self) -> &'static str {
        match self {
            Preset::Lazy => "lazy",
            Preset::Terse => "terse",
            Preset::Disciplined => "disciplined",
            Preset::Ship => "ship",
            Preset::TandemDefault => "tandem",
        }
    }

    pub fn from_name(s: &str) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| p.name().eq_ignore_ascii_case(s) || (s.eq_ignore_ascii_case("default") && *p == Preset::TandemDefault))
    }

    /// Skill ids in this preset. All exist in the catalog (checked by tests).
    pub fn skills(self) -> Vec<SkillId> {
        let ids: &[&str] = match self {
            Preset::Lazy => &["ponytail", "ponytail-review"],
            Preset::Terse => &["caveman", "caveman-commit", "caveman-review"],
            Preset::Disciplined => &[
                "brainstorming",
                "test-driven-development",
                "systematic-debugging",
                "verification-before-completion",
            ],
            Preset::Ship => &["gstack-review", "gstack-ship", "gstack-qa"],
            Preset::TandemDefault => {
                &["tandem-review", "tandem-db-safety", "tandem-worktree-etiquette", "tandem-harness"]
            }
        };
        ids.iter().map(|s| SkillId::from(*s)).collect()
    }
}
