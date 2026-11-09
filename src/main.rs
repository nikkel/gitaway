//! The `gitaway` binary. All the logic is in the library; see `gitaway::cli`.

fn main() {
    std::process::exit(gitaway::cli::main());
}
