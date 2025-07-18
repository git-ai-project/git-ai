use crate::error::GitAiError;
use git2::Repository;

pub fn run(_repo: &Repository) -> Result<(), GitAiError> {
    println!("hello world");
    Ok(())
}
