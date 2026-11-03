use vergen_gitcl::{Emitter, Gitcl};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let gitcl = Gitcl::all_git();
    Emitter::default().add_instructions(&gitcl)?.emit()?;
    Ok(())
}
