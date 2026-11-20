mod antigravity;
mod claude;
mod codex;
mod gemini;

use crate::model::{Report, Usage};

pub trait Provider: Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    /// "oauth" or "local".
    fn source(&self) -> &'static str;
    /// Cheap check that needs no network: are the provider's credentials (or app) present?
    fn signed_in(&self) -> bool;
    fn fetch(&self) -> Result<Usage, String>;

    fn report(&self) -> Report {
        match self.fetch() {
            Ok(usage) => Report::Ok(usage),
            Err(error) => Report::Failed {
                provider: self.id(),
                name: self.name(),
                error,
            },
        }
    }
}

pub fn all() -> Vec<Box<dyn Provider>> {
    vec![
        Box::new(codex::Codex),
        Box::new(claude::Claude),
        Box::new(antigravity::Antigravity),
        Box::new(gemini::Gemini),
    ]
}
