use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaVector {
    pub favors_safety_over_speed: f64,
    pub prefers_conciseness: f64,
    pub code_style_strictness: f64,
    pub forbidden_phrases: Vec<String>,
    pub moral_priorities: Vec<String>,
    pub language: String,
    pub frozen: bool,
    pub refuses_unsafe_code: bool,
}

impl PersonaVector {
    pub fn for_language(lang: &str) -> Self {
        match lang {
            "rust" => Self {
                favors_safety_over_speed: 0.9,
                prefers_conciseness: 0.8,
                code_style_strictness: 0.9,
                forbidden_phrases: vec!["大概可能".into(), "也许".into()],
                moral_priorities: vec!["安全性".into(), "性能".into()],
                language: "rust".into(),
                frozen: false,
                refuses_unsafe_code: true,
            },
            "python" => Self {
                favors_safety_over_speed: 0.6,
                prefers_conciseness: 0.7,
                code_style_strictness: 0.6,
                forbidden_phrases: vec![],
                moral_priorities: vec!["可读性".into(), "简洁".into()],
                language: "python".into(),
                frozen: false,
                refuses_unsafe_code: true,
            },
            "go" => Self {
                favors_safety_over_speed: 0.7,
                prefers_conciseness: 0.8,
                code_style_strictness: 0.8,
                forbidden_phrases: vec![],
                moral_priorities: vec!["简洁".into(), "性能".into()],
                language: "go".into(),
                frozen: false,
                refuses_unsafe_code: true,
            },
            _ => Self {
                favors_safety_over_speed: 0.7,
                prefers_conciseness: 0.7,
                code_style_strictness: 0.7,
                forbidden_phrases: vec![],
                moral_priorities: vec!["实用性".into()],
                language: lang.into(),
                frozen: false,
                refuses_unsafe_code: true,
            },
        }
    }

    pub fn generate_prompt_block(&self) -> String {
        format!(
            "## Persona\n\
             - Safety priority: {safety:.1} | Conciseness: {concise:.1} | Style strictness: {style:.1}\n\
             - Refuses unsafe code: {refuses_unsafe}\n\
             - Forbidden phrases: {forbidden}\n\
             - Value priorities: {values}",
            safety = self.favors_safety_over_speed,
            concise = self.prefers_conciseness,
            style = self.code_style_strictness,
            refuses_unsafe = self.refuses_unsafe_code,
            forbidden = if self.forbidden_phrases.is_empty() { "(none)".into() } else { self.forbidden_phrases.join(", ") },
            values = self.moral_priorities.join(", "),
        )
    }

    pub fn evolve(&mut self, signal: PersonaSignal, max_change: f64) {
        if self.frozen { return; }
        // refuses_unsafe_code is NEVER modified by evolution — safety is non-negotiable.
        match signal {
            PersonaSignal::MoreConcise => {
                self.prefers_conciseness = adjust(self.prefers_conciseness, 0.05, max_change);
            }
            PersonaSignal::MoreVerbose => {
                self.prefers_conciseness = adjust(self.prefers_conciseness, -0.05, max_change);
            }
            PersonaSignal::StricterStyle => {
                self.code_style_strictness = adjust(self.code_style_strictness, 0.05, max_change);
            }
            PersonaSignal::LooserStyle => {
                self.code_style_strictness = adjust(self.code_style_strictness, -0.05, max_change);
            }
            PersonaSignal::Safer => {
                self.favors_safety_over_speed = adjust(self.favors_safety_over_speed, 0.05, max_change);
            }
        }
    }
}

fn adjust(current: f64, delta: f64, max_change: f64) -> f64 {
    let change = delta.abs().min(max_change) * delta.signum();
    (current + change).clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Copy)]
pub enum PersonaSignal {
    MoreConcise,
    MoreVerbose,
    StricterStyle,
    LooserStyle,
    Safer,
}

impl fmt::Display for PersonaVector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Safety: {:.2} | Conciseness: {:.2} | Style: {:.2} | RefusesUnsafe: {} | Lang: {} | Frozen: {}",
            self.favors_safety_over_speed,
            self.prefers_conciseness,
            self.code_style_strictness,
            self.refuses_unsafe_code,
            self.language,
            self.frozen,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_persona_defaults() {
        let p = PersonaVector::for_language("rust");
        assert!((p.favors_safety_over_speed - 0.9).abs() < 0.01);
        assert!((p.code_style_strictness - 0.9).abs() < 0.01);
        assert!(p.refuses_unsafe_code);
    }

    #[test]
    fn evolve_respects_max_change() {
        let mut p = PersonaVector::for_language("rust");
        p.evolve(PersonaSignal::MoreConcise, 0.1);
        assert!(p.prefers_conciseness <= 0.9);
    }

    #[test]
    fn frozen_prevents_evolution() {
        let mut p = PersonaVector::for_language("rust");
        p.frozen = true;
        let before = p.prefers_conciseness;
        p.evolve(PersonaSignal::MoreConcise, 0.1);
        assert_eq!(p.prefers_conciseness, before);
    }

    #[test]
    fn refuses_unsafe_code_is_never_modified() {
        let mut p = PersonaVector::for_language("rust");
        assert!(p.refuses_unsafe_code);
        p.evolve(PersonaSignal::Safer, 0.5);
        assert!(p.refuses_unsafe_code);
        p.favors_safety_over_speed = 0.0;
        p.evolve(PersonaSignal::Safer, 0.5);
        assert!(p.refuses_unsafe_code);
    }
}
