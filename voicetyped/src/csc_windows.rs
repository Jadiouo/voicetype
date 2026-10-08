//! Windows CSC is attached to the shared owned process during desktop integration.
//! Keep the original text when no private spelling worker is available.
pub struct CscClient;
impl CscClient {
    pub fn from_env() -> Option<Self> { None }
    pub fn correct(&self, text: &str, _terms: &[String]) -> String { text.to_owned() }
}
