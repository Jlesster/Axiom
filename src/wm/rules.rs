// src/wm/rules.rs — Window rules engine.

#[derive(Debug, Clone)]
pub struct WindowRule {
    pub matcher: Matcher,
    pub effects: Vec<Effect>,
}

#[derive(Debug, Clone)]
pub enum Matcher {
    AppId(String),
    Title(String),
    Both { app_id: String, title: String },
    Always,
}

impl Matcher {
    pub fn matches(&self, app_id: &str, title: &str) -> bool {
        match self {
            Self::AppId(a) => a == app_id,
            Self::Title(t) => t == title,
            Self::Both {
                app_id: a,
                title: t,
            } => a == app_id && t == title,
            Self::Always => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Float,
    Workspace(usize),
    Size(i32, i32),
    Position(i32, i32),
    Opacity(f32),
    Sticky,
    NoDecoration,
    Scratchpad(String),
    InhibitIdle,
}

#[derive(Default, Clone)]
pub struct RuleEngine {
    pub rules: Vec<WindowRule>,
}

impl RuleEngine {
    pub fn add(&mut self, rule: WindowRule) {
        self.rules.push(rule);
    }

    pub fn evaluate(&self, app_id: &str, title: &str) -> Vec<Effect> {
        let mut effects = vec![];
        for rule in &self.rules {
            if rule.matcher.matches(app_id, title) {
                effects.extend(rule.effects.iter().cloned());
            }
        }
        effects
    }
}
