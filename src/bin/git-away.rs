//! The `git-away` binary, so `git away push` works: git runs `git-away` for
//! the unknown subcommand `away`. Identical to `gitaway`; see `gitaway::cli`.

fn main() {
    std::process::exit(gitaway::cli::main());
}
