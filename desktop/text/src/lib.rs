//! Shared deterministic text policy for the daemon and desktop vocabulary commands.
pub mod spelling;
pub mod spelling_policy;
pub mod traditional;
pub mod vocab;
pub use traditional::Traditional;

pub fn code_at(text: &str, start: usize, end: usize) -> bool {
    let left = text[..start].chars().next_back();
    let right = text[end..].chars().next();
    [left, right]
        .into_iter()
        .flatten()
        .any(|c| "_/@\\=<>+*()[]{}-".contains(c))
        || (left == Some('.')
            && text[..start]
                .chars()
                .rev()
                .nth(1)
                .is_some_and(|c| c.is_ascii_alphanumeric()))
        || (right == Some('.')
            && text[end..]
                .chars()
                .nth(1)
                .is_some_and(|c| c.is_ascii_alphanumeric()))
}
