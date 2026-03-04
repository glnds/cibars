use vergen_gitcl::{Emitter, GitclBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let gitcl = GitclBuilder::all_git()?;
    Emitter::default().add_instructions(&gitcl)?.emit()?;
    Ok(())
}
