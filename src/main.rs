mod config;

use config::Config;

fn main() {
    match Config::load() {
        Ok((config, _token)) => {
            println!("Profile: {}", config.aws_profile);
            println!("Region:  {}", config.region);
            println!("Repo:    {}", config.github_repo);
        }
        Err(e) => {
            eprintln!("Error: {e:#}");
            std::process::exit(1);
        }
    }
}
